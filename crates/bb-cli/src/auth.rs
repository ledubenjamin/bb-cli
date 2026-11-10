use crate::{
    args::{AuthCommand, AuthLoginArgs, Cli, TokenType},
    config::{self, Config, Store},
    output::{AuthView, Kind, Outcome},
};
use anyhow::{Context as _, Result, bail};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use std::{
    fmt,
    io::{self, IsTerminal, Write},
    time::{SystemTime, UNIX_EPOCH},
};
mod oauth;
mod vault;
use oauth::{consumer, refresh, web_login};
use vault::{KeyringVault, Vault, delete_profile, save_profile, stored_credential};

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct Credential {
    pub token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub expires_at: Option<u64>,
    pub token_type: TokenType,
}
impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("token_type", &self.token_type)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
pub fn selected_profile<'a>(cli: &'a Cli, config: &'a Config) -> Result<Option<&'a str>> {
    let context = cli.context.as_ref().or(config.active_context.as_ref());
    if let Some(context) = context {
        let context = config
            .contexts
            .get(context)
            .context("unknown context; use bb context list")?;
        if let Some(profile) = &context.profile {
            return Ok(Some(profile));
        }
    }
    Ok(config.active_profile.as_deref())
}
fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}

fn expires_soon(credential: &Credential) -> Result<bool> {
    let threshold = now()?.saturating_add(60);
    Ok(credential
        .expires_at
        .is_some_and(|expires| expires <= threshold))
}

pub async fn credential(
    cli: &Cli,
    store: &Store,
    config: &Config,
    force_refresh: bool,
) -> Result<Option<Credential>> {
    if let Ok(token) = std::env::var("BB_TOKEN") {
        if token.is_empty() {
            bail!("BB_TOKEN is empty");
        }
        if force_refresh {
            bail!(
                "BB_TOKEN credentials cannot be refreshed by bb; unset BB_TOKEN to refresh a stored OAuth profile"
            );
        }
        let token_type = match std::env::var("BB_TOKEN_TYPE").as_deref() {
            Ok("access") => TokenType::Access,
            Ok("oauth") => TokenType::Oauth,
            Ok("api") | Err(_) => TokenType::Api,
            _ => bail!("BB_TOKEN_TYPE must be api, access, or oauth"),
        };
        return Ok(Some(Credential {
            token: SecretString::from(token),
            refresh_token: None,
            expires_at: None,
            token_type,
        }));
    }
    let Some(profile) = selected_profile(cli, config)? else {
        return Ok(None);
    };
    config::valid_name(profile)?;
    let vault = KeyringVault {
        store: store.clone(),
        profile: profile.to_owned(),
    };
    if cli.dry_run {
        let credential = tokio::task::spawn_blocking(move || vault.read()).await??;
        if expires_soon(&credential)? {
            return Err(crate::app::Failure {code:"authentication_required",message:"stored token expires soon; run bb auth refresh before a dry-run that needs API reads".into(),exit:4}.into());
        }
        return Ok(Some(credential));
    }
    stored_credential(
        vault,
        store,
        profile,
        force_refresh,
        |credential| async move { refresh(config, &credential).await },
    )
    .await
    .map(Some)
}

pub async fn execute(app: &crate::app::App<'_>, command: &AuthCommand) -> Result<Outcome> {
    match command {
        AuthCommand::Login(args) => login(app, args).await,
        AuthCommand::Logout { profile } => logout(app, profile.as_deref()).await,
        AuthCommand::Switch { profile } => switch(app, profile).await,
        AuthCommand::SetupGit => setup_git(app).await,
        AuthCommand::GitCredential { operation } => git_credential(app, operation).await,
        AuthCommand::Status => inspect_credentials(app, Inspection::Status).await,
        AuthCommand::Doctor => inspect_credentials(app, Inspection::Doctor).await,
        AuthCommand::Refresh => inspect_credentials(app, Inspection::Refresh).await,
    }
}

#[derive(Debug, Clone, Copy)]
enum Inspection {
    Status,
    Doctor,
    Refresh,
}

async fn login(app: &crate::app::App<'_>, args: &AuthLoginArgs) -> Result<Outcome> {
    let cli = app.cli;
    let store = &app.store;
    let config = &app.config;
    let input = &app.input;
    let AuthLoginArgs {
        web,
        with_token,
        token_type,
        profile,
    } = args;
    if cli.dry_run {
        return Outcome::action(
            "auth login",
            vec![
                json!({"flow":if *web {"browser-oauth"} else {"token"},"profile":profile,"storage":"system-keyring"}),
            ],
            true,
        );
    }
    if std::env::var_os("BB_TOKEN").is_some() {
        bail!("BB_TOKEN already supplies credentials; unset it before saving a login");
    }
    let credential = if *web {
        if cli.non_interactive || !io::stdin().is_terminal() {
            bail!(
                "browser login requires an interactive terminal; use BB_TOKEN or --with-token in CI"
            );
        }
        web_login(config).await?
    } else {
        let token = if *with_token {
            SecretString::from(crate::input::stdin(crate::input::CREDENTIAL_LIMIT).await?)
        } else if !cli.non_interactive && io::stdin().is_terminal() {
            input.password("Bitbucket token: ").await?
        } else {
            bail!("provide --with-token or set BB_TOKEN in non-interactive environments");
        };
        let token = SecretString::from(token.expose_secret().trim().to_owned());
        if token.expose_secret().is_empty() {
            bail!("token is empty");
        }
        Credential {
            token,
            refresh_token: None,
            expires_at: None,
            token_type: *token_type,
        }
    };
    let client = crate::app::client(cli, Some(&credential))?;
    let user = if matches!(credential.token_type, TokenType::Access) {
        let repo = crate::git::resolve_repo(cli, config, store)
            .await
            .context("access-token login requires -R workspace/repository to validate access")?;
        client.repository(&repo).await?;
        None
    } else {
        Some(client.current_user().await?)
    };
    let profile = profile.clone().unwrap_or_else(|| {
        user.as_ref()
            .map(|u| u.uuid.trim_matches(['{', '}']).to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "default".into())
    });
    config::valid_name(&profile)?;
    let meta = config::Profile {
        token_type: credential.token_type,
        user: user.as_ref().map(|u| u.uuid.clone()),
    };
    let saved_type = credential.token_type;
    save_profile(
        KeyringVault {
            store: store.clone(),
            profile: profile.clone(),
        },
        store,
        &profile,
        credential,
        meta,
    )
    .await?;
    Outcome::resource(
        Kind::Auth,
        AuthView {
            profile,
            source: "keyring".into(),
            token_type: format!("{saved_type:?}").to_lowercase(),
            authenticated: true,
            user: user.map(serde_json::to_value).transpose()?,
            diagnostics: vec![],
        },
    )
}

async fn logout(app: &crate::app::App<'_>, profile: Option<&str>) -> Result<Outcome> {
    let cli = app.cli;
    let store = &app.store;
    let config = &app.config;
    let input = &app.input;
    let profile = profile
        .or(selected_profile(cli, config)?)
        .context("no stored profile selected")?;
    crate::app::confirm(
        cli,
        input,
        &format!("Remove local credentials for {profile}?"),
    )
    .await?;
    if cli.dry_run {
        return Outcome::action("auth logout", vec![json!({"profile":profile})], true);
    }
    delete_profile(
        KeyringVault {
            store: store.clone(),
            profile: profile.to_owned(),
        },
        store,
        profile,
    )
    .await?;
    Outcome::action("auth logout", vec![json!({"profile":profile})], false)
}

async fn switch(app: &crate::app::App<'_>, profile: &str) -> Result<Outcome> {
    let cli = app.cli;
    let store = &app.store;
    let config = &app.config;
    if !config.profiles.contains_key(profile) {
        bail!("unknown profile '{profile}'");
    }
    if !cli.dry_run {
        store
            .update(|config| {
                // A logout may have removed this profile after the command's snapshot.
                if !config.profiles.contains_key(profile) {
                    bail!("unknown profile '{profile}'");
                }
                config.active_profile = Some(profile.to_owned());
                config.active_context = None;
                Ok(())
            })
            .await?;
    }
    Outcome::action("auth switch", vec![json!({"profile":profile})], cli.dry_run)
}

async fn setup_git(app: &crate::app::App<'_>) -> Result<Outcome> {
    let cli = app.cli;
    let clear = vec![
        "config".to_owned(),
        "--global".into(),
        "--replace-all".into(),
        "credential.https://bitbucket.org.helper".into(),
        String::new(),
    ];
    let install = vec![
        "config".to_owned(),
        "--global".into(),
        "--add".into(),
        "credential.https://bitbucket.org.helper".into(),
        "!bb auth git-credential".into(),
    ];
    if !cli.dry_run {
        crate::git::run(&clear, false).await?;
        crate::git::run(&install, false).await.context(
            "Bitbucket helper was cleared but installation failed; run bb auth setup-git again",
        )?;
    }
    Outcome::action(
        "auth setup-git",
        vec![json!({"git":clear}), json!({"git":install})],
        cli.dry_run,
    )
}

async fn git_credential(app: &crate::app::App<'_>, operation: &str) -> Result<Outcome> {
    let cli = app.cli;
    let store = &app.store;
    let config = &app.config;
    if operation != "get" {
        return Ok(Outcome::text(""));
    }
    let request = crate::input::stdin(crate::input::CREDENTIAL_LIMIT).await?;
    let fields: std::collections::BTreeMap<_, _> = request
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    if fields.get("protocol") != Some(&"https") || fields.get("host") != Some(&"bitbucket.org") {
        return Ok(Outcome::text(""));
    }
    let credential = credential(cli, store, config, false)
        .await?
        .context("no Bitbucket credential configured")?;
    if credential
        .token
        .expose_secret()
        .chars()
        .any(char::is_control)
    {
        bail!("invalid token for Git credential protocol");
    }
    let username = if matches!(credential.token_type, TokenType::Api) {
        "x-bitbucket-api-token-auth"
    } else {
        "x-token-auth"
    };
    crate::blocking::isolated("bb-git-credential", move || {
        let mut output = io::stdout().lock();
        writeln!(
            output,
            "username={username}\npassword={}\n",
            credential.token.expose_secret()
        )?;
        output.flush()?;
        Ok(())
    })
    .await?;
    Ok(Outcome::text(""))
}

async fn inspect_credentials(app: &crate::app::App<'_>, inspection: Inspection) -> Result<Outcome> {
    let cli = app.cli;
    let store = &app.store;
    let config = &app.config;
    if cli.dry_run && matches!(inspection, Inspection::Refresh) {
        return Outcome::action("auth refresh", vec![], true);
    }
    let credential = credential(
        cli,
        store,
        config,
        matches!(inspection, Inspection::Refresh),
    )
    .await?;
    let mut diagnostics = Vec::new();
    let mut user = None;
    let mut authenticated = false;
    if let Some(credential) = &credential {
        let client = crate::app::client(cli, Some(credential))?;
        if matches!(credential.token_type, TokenType::Access) {
            if let Ok(repo) = crate::git::resolve_repo(cli, config, store).await {
                client.repository(&repo).await?;
                authenticated = true;
            } else {
                diagnostics
                    .push("set -R workspace/repository to validate this resource token".into());
            }
        } else {
            user = Some(serde_json::to_value(client.current_user().await?)?);
            authenticated = true;
        }
    } else {
        diagnostics.push("run bb auth login or set BB_TOKEN".into());
    }
    if matches!(inspection, Inspection::Doctor) {
        if consumer(config).is_err() {
            diagnostics.push("OAuth consumer is not configured".into());
        }
        if crate::git::run(&["--version".into()], false).await.is_err() {
            diagnostics.push("Git is not available".into());
        }
    }
    let mut outcome = Outcome::resource(
        Kind::Auth,
        AuthView {
            profile: selected_profile(cli, config)?
                .unwrap_or("environment")
                .into(),
            source: if std::env::var_os("BB_TOKEN").is_some() {
                "environment"
            } else {
                "keyring"
            }
            .into(),
            token_type: credential
                .as_ref()
                .map(|c| format!("{:?}", c.token_type).to_lowercase())
                .unwrap_or_default(),
            authenticated,
            user,
            diagnostics,
        },
    )?;
    if !authenticated {
        outcome.code = 4;
    }
    Ok(outcome)
}
