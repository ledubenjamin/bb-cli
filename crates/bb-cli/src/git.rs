use crate::{
    args::Cli,
    config::{Config, Store},
};
use anyhow::{Context as _, Result, bail};
use bb_api::RepoRef;
use std::collections::BTreeSet;
use tokio::process::Command;

pub async fn run(args: &[String], network: bool) -> Result<String> {
    run_with_context(args, network, None).await
}

pub async fn run_with_context(
    args: &[String],
    network: bool,
    context: Option<&str>,
) -> Result<String> {
    let mut command = Command::new("git");
    command.kill_on_drop(true).env("GIT_TERMINAL_PROMPT", "0");
    if network {
        command
            .env("GIT_ASKPASS", "")
            .env("SSH_ASKPASS_REQUIRE", "never");
        if std::env::var_os("GIT_SSH_COMMAND").is_none() && std::env::var_os("GIT_SSH").is_none() {
            command.env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes");
        }
        if let Some(context) = context {
            command.env("BB_CONTEXT", context);
        }
        let executable =
            std::env::current_exe().context("cannot locate bb for Git credential helper")?;
        let executable = executable.to_string_lossy();
        #[cfg(windows)]
        let executable = executable.replace('\\', "/");
        let helper = format!(
            "!{} --non-interactive auth git-credential",
            shell_quote(&executable)
        );
        // Empty the inherited helper list before installing bb for this one command.
        command.args([
            "-c",
            "credential.https://bitbucket.org.helper=",
            "-c",
            &format!("credential.https://bitbucket.org.helper={helper}"),
        ]);
    }
    let output = command
        .args(args)
        .output()
        .await
        .context("cannot run Git; install Git and add it to PATH")?;
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git {} failed: {}",
            args.first().map_or("", String::as_str),
            diagnostic.trim()
        );
    }
    Ok(String::from_utf8(output.stdout)
        .context("Git returned non-UTF-8 output")?
        .trim_end()
        .to_owned())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub async fn resolve_repo(cli: &Cli, config: &Config, _store: &Store) -> Result<RepoRef> {
    if let Some(repo) = &cli.repo {
        return Ok(repo.clone());
    }
    if let Ok(repo) = std::env::var("BITBUCKET_REPO_FULL_NAME")
        && !repo.is_empty()
    {
        return repo.parse().context("invalid BITBUCKET_REPO_FULL_NAME");
    }
    if in_repository().await? {
        if let Some(repo) = local_default().await? {
            return repo
                .parse()
                .context("invalid local bb.repository setting; run bb repo set-default");
        }
        let repos = remote_repositories().await?;
        if repos.len() == 1 {
            return repos
                .into_iter()
                .next()
                .context("missing Git remote")?
                .parse()
                .map_err(Into::into);
        }
        if repos.len() > 1 {
            bail!(
                "multiple Bitbucket repositories found ({}); use -R or bb repo set-default",
                repos.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
    }
    if let Some(name) = cli.context.as_ref().or(config.active_context.as_ref()) {
        let context = config
            .contexts
            .get(name)
            .context("unknown context; use bb context list")?;
        if let Some(repo) = &context.repository {
            return Ok(repo.clone());
        }
    }
    bail!("cannot determine repository; use -R workspace/repository, BB_REPO, or bb context create")
}

async fn inspect(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git")
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0")
        // Git uses exit 128 for both absent repositories and broken configuration.
        // Fix the diagnostic locale so only the documented absence message is ignored.
        .env("LC_ALL", "C")
        .args(args)
        .output()
        .await
}

async fn in_repository() -> Result<bool> {
    let output = match inspect(&["rev-parse", "--absolute-git-dir"]).await {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("cannot inspect the Git repository"),
    };
    if output.status.success() {
        return Ok(true);
    }
    let message = String::from_utf8_lossy(&output.stderr);
    if message.starts_with("fatal: not a git repository (or any of the parent directories):") {
        return Ok(false);
    }
    bail!("cannot inspect the Git repository: {}", message.trim())
}

async fn local_default() -> Result<Option<String>> {
    let output = inspect(&["config", "--local", "--get", "bb.repository"])
        .await
        .context("cannot read the local Git configuration")?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8(output.stdout)
                .context("Git returned non-UTF-8 output")?
                .trim_end()
                .to_owned(),
        ));
    }
    // Git config --get returns 1 for an absent key; other failures must not select a fallback.
    if output.status.code() == Some(1) && output.stderr.is_empty() {
        return Ok(None);
    }
    bail!(
        "cannot read the local Git configuration: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

pub async fn remote_repositories() -> Result<BTreeSet<String>> {
    let names = run(&["remote".into()], false).await?;
    let mut repos = BTreeSet::new();
    for name in names.lines() {
        let urls = run(
            &[
                "remote".into(),
                "get-url".into(),
                "--all".into(),
                name.into(),
            ],
            false,
        )
        .await?;
        for url in urls.lines() {
            if let Ok(repo) = url.parse::<RepoRef>() {
                repos.insert(repo.to_string());
            }
        }
    }
    Ok(repos)
}

pub async fn current_branch() -> Result<bb_api::BranchName> {
    run(
        &[
            "symbolic-ref".into(),
            "--quiet".into(),
            "--short".into(),
            "HEAD".into(),
        ],
        false,
    )
    .await
    .context("detached HEAD or no Git repository; supply --head explicitly")?
    .parse()
    .map_err(Into::into)
}

pub async fn require_clean(repo: &RepoRef) -> Result<()> {
    if !remote_repositories().await?.contains(&repo.to_string()) {
        bail!(
            "selected repository does not match a local Bitbucket remote; local Git operation refused"
        );
    }
    let status = run(&["status".into(), "--porcelain".into()], false).await?;
    if !status.is_empty() {
        bail!("working tree contains local changes; commit or stash them before checkout/rebase");
    }
    Ok(())
}

pub fn clone_url(repo: &RepoRef, ssh: bool) -> String {
    if ssh {
        format!("git@bitbucket.org:{repo}.git")
    } else {
        format!("https://bitbucket.org/{repo}.git")
    }
}
