use crate::args::TokenType;
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

// Keep cross-process lock contention interruptible without busy spinning.
const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub active_profile: Option<String>,
    pub active_context: Option<String>,
    pub profiles: BTreeMap<String, Profile>,
    pub contexts: BTreeMap<String, Context>,
    pub preferences: Preferences,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub token_type: TokenType,
    pub user: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Context {
    pub profile: Option<String>,
    pub workspace: Option<bb_api::Workspace>,
    pub repository: Option<bb_api::RepoRef>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub default_workspace: Option<bb_api::Workspace>,
    pub favorite_repositories: Vec<Favorite>,
    pub git_protocol: String,
    pub oauth_client_id: Option<String>,
    pub oauth_redirect_uri: String,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            git_protocol: "https".into(),
            default_workspace: None,
            favorite_repositories: Vec::new(),
            oauth_client_id: None,
            oauth_redirect_uri: "http://127.0.0.1:8765/callback".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Favorite {
    pub api_origin: String,
    pub repository: bb_api::RepoRef,
}

#[derive(Debug, Clone)]
pub struct Store {
    directory: PathBuf,
}
enum LockKey<'a> {
    Config,
    Profile(&'a str),
}

impl Store {
    #[cfg(test)]
    pub fn for_test(directory: PathBuf) -> Self {
        Self { directory }
    }
    pub fn discover() -> Result<Self> {
        let directory = if let Some(path) = std::env::var_os("BB_CONFIG_DIR") {
            PathBuf::from(path)
        } else {
            directories::ProjectDirs::from("", "", "bb")
                .context("cannot determine configuration directory; set BB_CONFIG_DIR")?
                .config_dir()
                .to_owned()
        };
        let directory = if directory.is_absolute() {
            directory
        } else {
            std::env::current_dir()?.join(directory)
        };
        Ok(Self { directory })
    }
    pub async fn load(&self) -> Result<Config> {
        let text = match tokio::fs::read_to_string(self.directory.join("config.toml")).await {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config::default());
            }
            Err(error) => return Err(error).context("cannot read bb configuration"),
        };
        toml::from_str(&text).context("invalid bb configuration; refusing to overwrite it")
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    /// Shares the historical OAuth lock with login and logout for this profile.
    pub async fn lock_profile(&self, profile: &str) -> Result<File> {
        self.lock(LockKey::Profile(profile)).await
    }

    async fn lock(&self, key: LockKey<'_>) -> Result<File> {
        let name = match key {
            LockKey::Config => "config".to_owned(),
            LockKey::Profile(profile) => {
                valid_name(profile)?;
                format!("oauth-{profile}")
            }
        };
        let directory = self.directory.clone();
        let file = tokio::task::spawn_blocking(move || open_lock_file(&directory, &name)).await??;
        // Cross-process contention yields to Ctrl+C instead of occupying a blocking worker.
        let mut waiting = false;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if !waiting {
                        tracing::debug!(name: "config.lock.wait", "waiting for configuration lock");
                        waiting = true;
                    }
                    tokio::time::sleep(RETRY_INTERVAL).await;
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(error).context("cannot lock bb configuration");
                }
            }
        }
    }
    pub async fn update<T>(&self, update: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
        let lock = self.lock(LockKey::Config).await?;
        let mut config = self.load().await?;
        let result = update(&mut config)?;
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            // The writer owns the guard even if its awaiting caller is cancelled.
            let _lock = lock;
            persist(&directory, &config)?;
            Ok(())
        })
        .await??;
        Ok(result)
    }

    /// Mutates credentials and their configuration under profile-then-config locks.
    /// Both guards move into the blocking writer, so cancellation cannot expose a
    /// half-finished mutation to another login, logout, or refresh operation.
    pub async fn update_profile<T: Send + 'static>(
        &self,
        profile: &str,
        update: impl FnOnce(&mut Config) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let profile_lock = self.lock_profile(profile).await?;
        let config_lock = self.lock(LockKey::Config).await?;
        let mut config = self.load().await?;
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || {
            let _guards = (profile_lock, config_lock);
            let result = update(&mut config)?;
            persist(&directory, &config)?;
            Ok(result)
        })
        .await?
    }
}

fn persist(directory: &Path, config: &Config) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(toml::to_string_pretty(config)?.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(directory.join("config.toml"))
        .context("cannot atomically save bb configuration")?;
    Ok(())
}

fn open_lock_file(directory: &Path, name: &str) -> Result<File> {
    fs::create_dir_all(directory).context("cannot create bb configuration directory")?;
    #[cfg(unix)]
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options.mode(0o600);
    Ok(options.open(directory.join(format!("{name}.lock")))?)
}

pub fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 100
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    {
        bail!(
            "profile/context names must contain 1–100 ASCII letters, digits, underscores, or hyphens"
        );
    }
    Ok(())
}
