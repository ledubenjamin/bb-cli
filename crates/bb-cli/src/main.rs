//! A GitHub CLI shaped interface to Bitbucket Cloud.

mod admin;
mod app;
mod args;
mod auth;
mod blocking;
mod catalog;
mod config;
mod discovery;
mod favorites;
mod git;
mod input;
mod jq;
mod log_cache;
mod logs;
mod output;
mod pipeline_status;
mod pipelines;
mod pull_requests;
mod raw_api;
mod repos;
mod skills;
mod tui;
mod variables;

use std::{
    io::{self, Write},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches};
use serde_json::json;

use args::{Cli, Command};
use output::Kind;

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    let argv: Vec<_> = std::env::args_os().collect();
    let json_requested = argv
        .iter()
        .any(|arg| arg == "--json" || arg.to_string_lossy().starts_with("--json="));
    let result = run(argv);
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if error.chain().any(|source| {
                source
                    .downcast_ref::<io::Error>()
                    .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
            }) {
                return ExitCode::SUCCESS;
            }
            let (exit, code) = classify(&error);
            let message = format!("{error:#}");
            let message = match std::env::var("BB_TOKEN") {
                Ok(token) if !token.is_empty() => message.replace(&token, "[REDACTED]"),
                _ => message,
            };
            let mut stderr = io::stderr().lock();
            if json_requested {
                let _ = serde_json::to_writer(
                    &mut stderr,
                    &json!({"error":{"code":code,"message":message},"exitCode":exit}),
                );
                let _ = writeln!(stderr);
            } else {
                let clean: String = message
                    .chars()
                    .flat_map(|c| {
                        if c.is_control() && c != '\n' && c != '\t' {
                            c.escape_default().collect::<Vec<_>>()
                        } else {
                            vec![c]
                        }
                    })
                    .collect();
                let _ = writeln!(stderr, "bb: {clean}");
            }
            ExitCode::from(exit)
        }
    }
}

fn run(argv: Vec<std::ffi::OsString>) -> Result<u8> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start async runtime")?;
    let input = input::Input::default();
    let result = runtime.block_on(async {
        tokio::select! {
            biased;
            signal = tokio::signal::ctrl_c() => {
                signal.context("cannot listen for interruption")?;
                Err(app::Failure {
                    code: "cancelled",
                    message: "interrupted; an in-flight remote mutation may still complete; inspect its state before retrying".into(),
                    exit: 2,
                }.into())
            }
            result = run_command(argv, &input) => result,
        }
    });
    input.cancel();
    // Allow cooperative prompts to restore the terminal, without waiting indefinitely
    // for an OS keyring or filesystem call. The CLI exits immediately after cleanup.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}

async fn run_command(argv: Vec<std::ffi::OsString>, input: &input::Input) -> Result<u8> {
    let discovery = argv.iter().enumerate().any(|(i, arg)| {
        arg == "--json="
            || arg == "--json"
                && argv
                    .get(i + 1)
                    .is_none_or(|next| next.to_string_lossy().starts_with('-'))
    });
    let matches = match Cli::command()
        .ignore_errors(discovery)
        .try_get_matches_from(argv)
    {
        Ok(matches) => matches,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            return blocking::isolated("bb-help", move || {
                error.print()?;
                Ok(0)
            })
            .await;
        }
        Err(error) => {
            return Err(app::Failure {
                code: "invalid_arguments",
                message: error.to_string(),
                exit: 1,
            }
            .into());
        }
    };
    let path = catalog::command_path(&matches);
    if discovery && path == ["tui"] {
        bail!(
            "bb tui is interactive and does not support --json; use bb logs search or bb run list"
        );
    }
    let contract = catalog::spec(&path);
    if discovery {
        // Required arguments do not apply to offline field discovery.
        let kind = if matches.get_flag("dry_run") && contract.effect != "read" {
            Kind::Action
        } else {
            output_kind_from_matches(&matches, contract.kind)
        };
        let value = json!({"command":format!("bb {}", path.join(" ")),"fields":output::fields(kind),"schema":output::schema(kind)});
        let outcome = output::Outcome::new(Kind::Raw, value);
        output::render(outcome, Some("*".into()), None).await?;
        return Ok(0);
    }
    let cli = Cli::from_arg_matches(&matches)?;
    if let Command::Tui(args) = &cli.command {
        tui::validate(&cli)?;
        let app = app::App::new(&cli, input.clone()).await?;
        return tui::run(&app, args).await;
    }
    if let Command::Schema { command } = &cli.command {
        let outcome = output::Outcome::new(Kind::Raw, catalog::describe(command)?);
        output::render(outcome, Some("*".into()), cli.jq).await?;
        return Ok(0);
    }
    if let Command::Completion { shell } = cli.command {
        return blocking::isolated("bb-completion", move || {
            clap_complete::generate(shell, &mut Cli::command(), "bb", &mut io::stdout());
            Ok(0)
        })
        .await;
    }
    let kind =
        if cli.dry_run && (contract.effect != "read" || matches!(cli.command, Command::Api(_))) {
            Kind::Action
        } else {
            output_kind_from_matches(&matches, contract.kind)
        };
    prepare_output(&cli, kind).await?;
    let outcome = if let Command::Skills { command } = &cli.command {
        skills::execute(command.clone(), cli.dry_run).await?
    } else {
        let app = app::App::new(&cli, input.clone()).await?;
        app.execute().await?
    };
    let code = outcome.code;
    let selection = cli.json.clone().or_else(|| {
        (cli.jq.is_some() && matches!(cli.command, Command::Api(_))).then(|| "*".into())
    });
    output::render(outcome, selection, cli.jq.clone()).await.with_context(|| {
        if !cli.dry_run && matches!(contract.effect, "remote-write" | "local-write" | "dynamic") {
            "command completed but output rendering failed; inspect state before repeating the action"
        } else {
            "cannot render command output"
        }
    })?;
    Ok(code)
}

async fn prepare_output(cli: &Cli, kind: Kind) -> Result<()> {
    if let Some(fields) = &cli.json {
        output::validate_fields(kind, fields)?;
    }
    if cli.jq.is_some() && cli.json.is_none() && !matches!(cli.command, Command::Api(_)) {
        bail!("--jq requires --json outside bb api");
    }
    if let Some(program) = &cli.jq {
        let program = program.clone();
        blocking::isolated("bb-jq-compile", move || jq::validate(&program)).await?;
    }
    if cli.verbose {
        tracing_subscriber::fmt()
            .with_writer(io::stderr)
            .with_target(false)
            .with_ansi(false)
            .with_env_filter("bb_api=debug,bb=debug")
            .try_init()
            .map_err(|error| anyhow::anyhow!("cannot initialize diagnostics: {error}"))?;
    }
    Ok(())
}

fn output_kind_from_matches(matches: &clap::ArgMatches, default: Kind) -> Kind {
    let mut current = matches;
    while let Some((_, child)) = current.subcommand() {
        current = child;
    }
    if current
        .try_get_one::<bool>("stat")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
    {
        Kind::File
    } else if current
        .try_get_one::<bool>("log")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
    {
        Kind::Text
    } else {
        default
    }
}

fn classify(error: &anyhow::Error) -> (u8, &'static str) {
    if let Some(failure) = error.downcast_ref::<app::Failure>() {
        return (failure.exit, failure.code);
    }
    if let Some(error) = error.downcast_ref::<bb_api::Error>() {
        if error.is_authentication() {
            return (4, "authentication_required");
        }
        if error.is_forbidden() {
            return (1, "permission_denied");
        }
        if error.is_not_found() {
            return (1, "not_found");
        }
        if error.is_rate_limited() {
            return (1, "rate_limited");
        }
        if error.is_transport() {
            return (1, "transport_error");
        }
        if error.is_invalid_input() {
            return (1, "invalid_arguments");
        }
        return (1, "api_error");
    }
    (1, "error")
}
