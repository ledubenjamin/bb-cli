use crate::{
    args::{BodyArgs, Cli, Command, ConfigCommand, ContextCommand, StatusCommand},
    auth,
    config::{self, Config, Store},
    input::{self, Input},
    output::{ContextView, Kind, Outcome},
};
use anyhow::{Context as _, Result, bail};
use bb_api::{Client, Method, RepoRef, Workspace};
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use std::{
    io::{self, IsTerminal},
    time::Duration,
};
use tokio::sync::OnceCell;

#[derive(Debug)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
    pub exit: u8,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Failure {}

#[derive(Debug)]
pub struct App<'a> {
    pub cli: &'a Cli,
    pub store: Store,
    pub config: Config,
    pub input: Input,
    client: OnceCell<Client>,
    repo: OnceCell<RepoRef>,
}
impl<'a> App<'a> {
    pub async fn new(cli: &'a Cli, input: Input) -> Result<Self> {
        let store = Store::discover()?;
        let config = store.load().await?;
        // Validate explicitly selected contexts even for commands with a repository override.
        if let Some(name) = &cli.context
            && !config.contexts.contains_key(name)
        {
            bail!("unknown context '{name}'");
        }
        Ok(Self {
            cli,
            store,
            config,
            input,
            client: OnceCell::new(),
            repo: OnceCell::new(),
        })
    }
    pub async fn confirm(&self, prompt: &str) -> Result<()> {
        confirm(self.cli, &self.input, prompt).await
    }

    pub async fn client(&self) -> Result<&Client> {
        self.client
            .get_or_try_init(|| async {
                let credentials =
                    auth::credential(self.cli, &self.store, &self.config, false).await?;
                let credentials = credentials.ok_or_else(|| Failure {
                    code: "authentication_required",
                    message: "run bb auth login or set BB_TOKEN".into(),
                    exit: 4,
                })?;
                client(self.cli, Some(&credentials))
            })
            .await
    }
    pub async fn repo(&self) -> Result<&RepoRef> {
        self.repo
            .get_or_try_init(|| crate::git::resolve_repo(self.cli, &self.config, &self.store))
            .await
    }
    pub async fn workspace(&self) -> Result<Workspace> {
        if let Some(workspace) = &self.cli.workspace {
            return Ok(workspace.clone());
        }
        if let Some(repo) = &self.cli.repo {
            return Ok(repo.workspace().clone());
        }
        if let Some(name) = self
            .cli
            .context
            .as_ref()
            .or(self.config.active_context.as_ref())
            && let Some(context) = self.config.contexts.get(name)
        {
            if let Some(workspace) = &context.workspace {
                return Ok(workspace.clone());
            }
            if let Some(repo) = &context.repository {
                return Ok(repo.workspace().clone());
            }
        }
        if let Some(workspace) = &self.config.preferences.default_workspace {
            return Ok(workspace.clone());
        }
        self.repo()
            .await
            .map(|repo| repo.workspace().clone())
            .context("cannot determine workspace; supply --workspace or BB_WORKSPACE")
    }
    pub fn ssh(&self) -> bool {
        self.config.preferences.git_protocol == "ssh"
    }

    pub async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        destructive: bool,
    ) -> Result<Value> {
        if self.cli.dry_run {
            bail!("internal error: attempted to send a mutation during --dry-run");
        }
        if destructive {
            self.confirm(&format!(
                "{} {path}?",
                serde_json::to_value(method)?.as_str().unwrap_or("Change")
            ))
            .await?;
        }
        let response = self
            .client()
            .await?
            .raw(method, path, &[], body, &[])
            .await?;
        if response.body.is_empty() {
            Ok(Value::Null)
        } else {
            serde_json::from_slice(&response.body).context("mutation succeeded but its response was not valid JSON; inspect remote state before retrying")
        }
    }
    pub fn plan(
        operation: &str,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Outcome> {
        Outcome::action(
            operation,
            vec![json!({"method":method,"path":path,"body":body.map(redact)})],
            true,
        )
    }
    pub async fn mutation(
        &self,
        operation: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
        kind: Kind,
        destructive: bool,
    ) -> Result<Outcome> {
        if self.cli.dry_run {
            return Self::plan(operation, method, path, body.as_ref());
        }
        let value = self.send(method, path, body.as_ref(), destructive).await?;
        if value.is_null() || kind == Kind::Action {
            Outcome::action(operation, vec![json!({"method":method,"path":path})], false)
        } else {
            Outcome::resource(kind, value).context("mutation succeeded but its result could not be decoded; inspect remote state before retrying")
        }
    }
    pub async fn execute(&self) -> Result<Outcome> {
        match &self.cli.command {
            Command::Auth { command } => auth::execute(self, command).await,
            Command::Context { command } => self.context(command).await,
            Command::Config { command } => self.preferences(command).await,
            Command::Repo { command } => crate::repos::repository(self, command).await,
            Command::Pr { command } => crate::pull_requests::execute(self, command).await,
            Command::Branch { command } => crate::repos::branch(self, command).await,
            Command::Commit { command } => crate::pull_requests::commit(self, command).await,
            Command::Project { command } => crate::admin::project(self, command).await,
            Command::Perms { command } => crate::admin::permissions(self, command).await,
            Command::Run { command } => crate::pipelines::execute(self, command).await,
            Command::Logs { command } => crate::logs::execute(self, command).await,
            Command::Tui(_) => bail!("interactive commands must own the terminal"),
            Command::Workflow { command } => crate::pipelines::workflow(self, command).await,
            Command::Variable { command } => crate::variables::execute(self, command).await,
            Command::Webhook { command } => crate::admin::webhook(self, command).await,
            Command::Status {
                command: Some(StatusCommand::RateLimit),
            } => {
                let response = self
                    .client()
                    .await?
                    .raw(Method::Get, &self.repo().await?.path(), &[], None, &[])
                    .await?;
                let headers: serde_json::Map<_, _> = response
                    .headers
                    .into_iter()
                    .filter(|(name, _)| name.contains("ratelimit") || name == "retry-after")
                    .map(|(key, value)| (key, Value::String(value)))
                    .collect();
                Ok(Outcome::new(Kind::RateLimit, Value::Object(headers)))
            }
            Command::Status { command: None } => crate::pull_requests::status(self).await,
            Command::Browse { path } => {
                let repo = self.repo().await?;
                let mut url = url::Url::parse(&repo.web_url())?;
                if let Some(path) = path {
                    let mut parts = url
                        .path_segments_mut()
                        .map_err(|()| anyhow::anyhow!("invalid repository URL"))?;
                    for part in path.split('/') {
                        if part == ".." {
                            bail!("browse path cannot contain '..'");
                        }
                        parts.push(part);
                    }
                }
                if !self.cli.dry_run {
                    open_browser(url.as_str()).await?;
                }
                Outcome::action(
                    "browse",
                    vec![json!({"url":url.as_str()})],
                    self.cli.dry_run,
                )
            }
            Command::Api(args) => crate::raw_api::execute(self, args).await,
            Command::Schema { .. } | Command::Completion { .. } | Command::Skills { .. } => {
                bail!("offline commands must be handled before application initialization")
            }
        }
    }

    async fn context(&self, command: &ContextCommand) -> Result<Outcome> {
        match command {
            ContextCommand::List => {
                let contexts: Vec<_> = self
                    .config
                    .contexts
                    .iter()
                    .map(|(name, context)| ContextView {
                        name: name.clone(),
                        profile: context.profile.clone(),
                        workspace: context.workspace.as_ref().map(ToString::to_string),
                        repository: context.repository.as_ref().map(ToString::to_string),
                        active: self.config.active_context.as_ref() == Some(name),
                    })
                    .collect();
                Outcome::resource(Kind::Context, contexts)
            }
            ContextCommand::Create { name, profile } => {
                config::valid_name(name)?;
                if self.config.contexts.contains_key(name) {
                    bail!("context '{name}' already exists");
                }
                let profile = profile
                    .clone()
                    .or_else(|| self.config.active_profile.clone());
                if profile
                    .as_ref()
                    .is_some_and(|name| !self.config.profiles.contains_key(name))
                {
                    bail!("unknown credential profile");
                }
                let context = config::Context {
                    profile,
                    workspace: self.cli.workspace.clone(),
                    repository: self.cli.repo.clone(),
                };
                if !self.cli.dry_run {
                    self.store
                        .update(|config| {
                            if config.contexts.contains_key(name) {
                                bail!("context '{name}' already exists");
                            }
                            config.contexts.insert(name.clone(), context);
                            config.active_context = Some(name.clone());
                            Ok(())
                        })
                        .await?;
                }
                Outcome::action(
                    "context create",
                    vec![json!({"name":name})],
                    self.cli.dry_run,
                )
            }
            ContextCommand::Use { name } => {
                if !self.config.contexts.contains_key(name) {
                    bail!("unknown context '{name}'");
                }
                if !self.cli.dry_run {
                    self.store
                        .update(|config| {
                            config.active_context = Some(name.clone());
                            Ok(())
                        })
                        .await?;
                }
                Outcome::action("context use", vec![json!({"name":name})], self.cli.dry_run)
            }
            ContextCommand::Delete { name } => {
                if !self.config.contexts.contains_key(name) {
                    bail!("unknown context '{name}'");
                }
                self.confirm(&format!("Delete context {name}?")).await?;
                if !self.cli.dry_run {
                    self.store
                        .update(|config| {
                            config.contexts.remove(name);
                            if config.active_context.as_ref() == Some(name) {
                                config.active_context = None;
                            }
                            Ok(())
                        })
                        .await?;
                }
                Outcome::action(
                    "context delete",
                    vec![json!({"name":name})],
                    self.cli.dry_run,
                )
            }
        }
    }
    async fn preferences(&self, command: &ConfigCommand) -> Result<Outcome> {
        let values = serde_json::to_value(&self.config.preferences)?;
        match command {
            ConfigCommand::List => Ok(Outcome::new(
                Kind::Config,
                crate::output::camel_value(values),
            )),
            ConfigCommand::Get { key } => {
                let value = values
                    .get(key)
                    .with_context(|| format!("unknown preference '{key}'"))?;
                Ok(Outcome::new(Kind::Config, json!({key:value})))
            }
            ConfigCommand::Set { key, value } => {
                let update = |config: &mut Config| -> Result<()> {
                    match key.as_str() {
                        "default_workspace" => {
                            config.preferences.default_workspace = Some(value.parse()?);
                        }
                        "git_protocol" if matches!(value.as_str(), "https" | "ssh") => {
                            config.preferences.git_protocol.clone_from(value);
                        }
                        "oauth_client_id" => {
                            config.preferences.oauth_client_id = Some(value.clone());
                        }
                        "oauth_redirect_uri" => {
                            url::Url::parse(value).context("invalid redirect URL")?;
                            config.preferences.oauth_redirect_uri.clone_from(value);
                        }
                        _ => bail!(
                            "expected default_workspace, git_protocol=https|ssh, oauth_client_id, or oauth_redirect_uri"
                        ),
                    }
                    Ok(())
                };
                if self.cli.dry_run {
                    update(&mut self.config.clone())?;
                    return Outcome::action("config set", vec![json!({key:value})], true);
                }
                self.store.update(update).await?;
                Ok(Outcome::new(Kind::Config, json!({key:value})))
            }
        }
    }
}

pub fn client(cli: &Cli, credential: Option<&auth::Credential>) -> Result<Client> {
    let mut builder = Client::builder().timeout(Duration::from_secs(cli.http_timeout));
    if let Some(credential) = credential {
        builder = builder.token(credential.token.expose_secret().to_owned());
    }
    if let Ok(base) = std::env::var("BB_API_URL") {
        let url = url::Url::parse(&base)?;
        let ip: std::net::IpAddr = url
            .host_str()
            .unwrap_or("")
            .trim_matches(['[', ']'])
            .parse()
            .context("BB_API_URL is only supported for numeric loopback test servers")?;
        if !ip.is_loopback() {
            bail!("BB_API_URL is only supported for loopback test servers");
        }
        builder = builder.base_url(base);
    }
    Ok(builder.build()?)
}

pub fn cancelled() -> anyhow::Error {
    Failure {
        code: "cancelled",
        message: "operation cancelled".into(),
        exit: 2,
    }
    .into()
}

pub async fn confirm(cli: &Cli, input: &Input, prompt: &str) -> Result<()> {
    if cli.yes || cli.dry_run {
        return Ok(());
    }
    if cli.non_interactive || !io::stdin().is_terminal() {
        bail!("this operation requires --yes in non-interactive mode; use --dry-run to inspect it");
    }
    if !input.confirm(prompt).await? {
        return Err(cancelled());
    }
    Ok(())
}

pub async fn body(args: &BodyArgs) -> Result<Option<String>> {
    match (&args.body, &args.body_file) {
        (Some(body), _) => Ok(Some(body.clone())),
        (_, Some(path)) if path == "-" => Ok(Some(input::stdin(input::BODY_LIMIT).await?)),
        (_, Some(path)) => Ok(Some(
            input::file(std::path::Path::new(path), input::BODY_LIMIT).await?,
        )),
        _ => Ok(None),
    }
}
pub fn required(value: Option<String>, name: &str) -> Result<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{name} is required; supply the corresponding option explicitly"))
}
pub fn encode(value: &str) -> String {
    use std::fmt::Write;
    value
        .as_bytes()
        .iter()
        .fold(String::new(), |mut text, &byte| {
            if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                text.push(char::from(byte));
            } else {
                let _ = write!(text, "%{byte:02X}");
            }
            text
        })
}
pub fn quoted(value: &str) -> String {
    serde_json::Value::String(value.into()).to_string()
}
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let sensitive = matches!(
                        key.as_str(),
                        "token" | "access_token" | "refresh_token" | "client_secret" | "password"
                    ) || key == "value"
                        && object.get("secured") == Some(&Value::Bool(true));
                    (
                        key.clone(),
                        if sensitive {
                            json!("[REDACTED]")
                        } else {
                            redact(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact).collect()),
        value => value.clone(),
    }
}
pub async fn open_browser(value: &str) -> Result<()> {
    let url = url::Url::parse(value)?;
    if url.scheme() != "https"
        || url.host_str() != Some("bitbucket.org")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("browser target must be an HTTPS Bitbucket Cloud URL");
    }
    tokio::task::spawn_blocking(move || webbrowser::open(url.as_str()))
        .await?
        .context("cannot launch a browser")
}
