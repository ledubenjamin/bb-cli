use std::fmt::Write as _;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bb_api::{
    Method, RepoRef,
    models::{Run, Step},
};
use serde_json::{Value, json};

use crate::{
    app::{self, App},
    args::{RunCommand, WatchArgs, WorkflowCommand},
    output::{Kind, Outcome},
    pipeline_status,
};

fn run_path(repo: &RepoRef, id: &str) -> Result<String> {
    let id = uuid::Uuid::parse_str(id).context("pipeline ID must be a UUID or build number")?;
    Ok(format!(
        "{}/pipelines/{}",
        repo.path(),
        app::encode(&format!("{{{id}}}"))
    ))
}

async fn resolve(app: &App<'_>, repo: &RepoRef, id: &str) -> Result<Run> {
    let client = app.client().await?;
    if let Ok(number) = id.parse::<u64>() {
        if number == 0 {
            bail!("build number must be positive");
        }
        let runs: Vec<Run> = client
            .collection(
                &format!("{}/pipelines", repo.path()),
                &[("q".into(), format!("build_number={number}"))],
                2,
            )
            .await?;
        match runs.as_slice() {
            [run] => Ok(run.clone()),
            [] => bail!("pipeline #{number} was not found"),
            _ => bail!("pipeline build number is ambiguous; supply a UUID"),
        }
    } else {
        Ok(client.resource(&run_path(repo, id)?).await?)
    }
}

pub async fn execute(app: &App<'_>, command: &RunCommand) -> Result<Outcome> {
    let repo = app.repo().await?;
    match command {
        RunCommand::List(list) => {
            let runs: Vec<Run> = app
                .client()
                .await?
                .collection(
                    &format!("{}/pipelines", repo.path()),
                    &[("sort".into(), "-created_on".into())],
                    list.limit as usize,
                )
                .await?;
            Outcome::resource(Kind::Run, runs)
        }
        RunCommand::View { id, log, step, web } => {
            let run = resolve(app, repo, id).await?;
            if *web {
                let url = run.links.get("html").map_or_else(
                    || format!("{}/pipelines/results/{}", repo.web_url(), run.build_number),
                    |link| link.href.clone(),
                );
                if !app.cli.dry_run {
                    app::open_browser(&url).await?;
                }
            }
            if *log {
                logs(app, repo, &run, step.as_deref()).await
            } else {
                let wants_steps = app.cli.json.as_ref().is_some_and(|fields| {
                    fields == "*" || fields.split(',').any(|field| field == "steps")
                });
                let steps = if wants_steps {
                    Some(
                        app.client()
                            .await?
                            .collection::<Step>(
                                &format!("{}/steps", run_path(repo, &run.uuid)?),
                                &[],
                                usize::MAX,
                            )
                            .await?,
                    )
                } else {
                    None
                };
                let mut outcome = Outcome::resource(Kind::Run, run)?;
                outcome.value["steps"] = crate::output::camel_value(serde_json::to_value(steps)?);
                Ok(outcome)
            }
        }
        RunCommand::Watch { id, watch } => {
            let run = resolve(app, repo, id).await?;
            watch_run(app, repo, run, watch).await
        }
    }
}

async fn watch_run(
    app: &App<'_>,
    repo: &RepoRef,
    mut run: Run,
    watch: &WatchArgs,
) -> Result<Outcome> {
    let deadline = (watch.timeout != 0)
        .then(|| tokio::time::Instant::now() + Duration::from_secs(watch.timeout));
    let path = run_path(repo, &run.uuid)?;
    loop {
        if let Some(code) = pipeline_status::completion_code(&run.state) {
            let mut outcome = Outcome::resource(Kind::Run, run)?;
            outcome.code = code;
            return Ok(outcome);
        }
        if watch.fail_fast {
            let inspect = async {
                let steps: Vec<Step> = app
                    .client()
                    .await?
                    .collection(&format!("{path}/steps"), &[], usize::MAX)
                    .await?;
                Ok::<_, anyhow::Error>(
                    steps
                        .iter()
                        .any(|step| pipeline_status::fails_fast(&step.state)),
                )
            };
            let failed = if let Some(deadline) = deadline {
                if let Ok(result) = tokio::time::timeout_at(deadline, inspect).await {
                    result?
                } else {
                    let mut outcome = Outcome::resource(Kind::Run, run)?;
                    outcome.code = 8;
                    return Ok(outcome);
                }
            } else {
                inspect.await?
            };
            if failed {
                let mut outcome = Outcome::resource(Kind::Run, run)?;
                outcome.code = 1;
                return Ok(outcome);
            }
        }
        if app.cli.json.is_none() {
            eprintln!("Pipeline #{}: {}", run.build_number, run.state.name);
        }
        let poll = async {
            tokio::time::sleep(Duration::from_secs(watch.interval)).await;
            Ok::<Run, anyhow::Error>(app.client().await?.resource(&path).await?)
        };
        run = if let Some(deadline) = deadline {
            if let Ok(result) = tokio::time::timeout_at(deadline, poll).await {
                result?
            } else {
                let mut outcome = Outcome::resource(Kind::Run, run)?;
                outcome.code = 8;
                return Ok(outcome);
            }
        } else {
            poll.await?
        };
    }
}

async fn logs(app: &App<'_>, repo: &RepoRef, run: &Run, selected: Option<&str>) -> Result<Outcome> {
    let path = run_path(repo, &run.uuid)?;
    let mut steps: Vec<Step> = app
        .client()
        .await?
        .collection(&format!("{path}/steps"), &[], usize::MAX)
        .await?;
    if let Some(selected) = selected {
        let uuid = uuid::Uuid::parse_str(selected).ok();
        steps.retain(|step| {
            step.name == selected
                || uuid.is_some_and(|uuid| uuid::Uuid::parse_str(&step.uuid).ok() == Some(uuid))
        });
        if steps.len() != 1 {
            bail!("--step must identify exactly one step by name or UUID");
        }
    }
    if steps.is_empty() {
        bail!("pipeline has no steps yet");
    }
    let mut combined = String::new();
    for step in steps {
        let log_path = format!("{path}/steps/{}/log", app::encode(&step.uuid));
        if app.cli.json.is_none() {
            eprintln!("Step: {} ({})", step.name, step.uuid);
            let mut stdout = tokio::io::stdout();
            app.client().await?.download(&log_path, &mut stdout).await?;
        } else {
            let mut bytes = LimitedBuffer::default();
            app.client().await?.download(&log_path, &mut bytes).await?;
            writeln!(combined, "=== {} ===", step.name)?;
            combined.push_str(
                std::str::from_utf8(&bytes.0)
                    .context("pipeline log is not UTF-8; omit --json to stream raw bytes")?,
            );
            if combined.len() > 64 * 1024 * 1024 {
                bail!("combined logs exceed 64 MiB; omit --json to stream logs");
            }
        }
    }
    Ok(Outcome::text(combined))
}

#[derive(Debug, Default)]
struct LimitedBuffer(Vec<u8>);

impl tokio::io::AsyncWrite for LimitedBuffer {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.0.len().saturating_add(buf.len()) > 64 * 1024 * 1024 {
            return std::task::Poll::Ready(Err(std::io::Error::other(
                "log exceeds 64 MiB; omit --json to stream it",
            )));
        }
        self.0.extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

pub async fn workflow(app: &App<'_>, command: &WorkflowCommand) -> Result<Outcome> {
    let WorkflowCommand::Run {
        name,
        reference,
        tag,
        variables,
        watch,
    } = command;
    let repo = app.repo().await?;
    let reference = match reference {
        Some(reference) => reference.clone(),
        None if *tag => bail!("--tag requires --ref"),
        None => crate::git::current_branch().await?.to_string(),
    };
    if reference.is_empty() || reference.chars().any(char::is_control) {
        bail!("pipeline ref must be nonempty and contain no control characters");
    }
    let mut target = json!({"type":"pipeline_ref_target", "ref_type":if *tag {"tag"} else {"branch"}, "ref_name":reference});
    if let Some(name) = name {
        if name.trim().is_empty() {
            bail!("custom pipeline name must not be empty");
        }
        target["selector"] = json!({"type":"custom", "pattern":name});
    }
    let mut seen = std::collections::HashSet::new();
    let variables = variables
        .iter()
        .map(|entry| {
            let (key, value) = crate::raw_api::pair(entry)?;
            crate::variables::validate_key(&key)?;
            if !seen.insert(key.clone()) {
                bail!("duplicate pipeline variable '{key}'");
            }
            // Runtime variables are treated as secrets in plans and logs.
            Ok(json!({"key":key, "value":value, "secured":true}))
        })
        .collect::<Result<Vec<Value>>>()?;
    let payload = json!({"target":target, "variables":variables});
    let path = format!("{}/pipelines", repo.path());
    if app.cli.dry_run {
        return App::plan("workflow run", Method::Post, &path, Some(&payload));
    }
    let value = app.send(Method::Post, &path, Some(&payload), false).await?;
    let run: Run = serde_json::from_value(value).context(
        "pipeline was triggered but its response was invalid; inspect runs before retrying",
    )?;
    if watch.watch {
        watch_run(app, repo, run, watch).await
    } else {
        Outcome::resource(Kind::Run, run)
    }
}
