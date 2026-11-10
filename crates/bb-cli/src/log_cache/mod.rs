//! Shared, bounded local storage for the terminal and machine-readable log search.

mod database;
mod text;

use anyhow::{Context, Result, bail};
use database::Database;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot};

pub const BLOCK_SIZE: usize = 65_536;
pub const QUOTA: u64 = 500 * 1024 * 1024;
const RETENTION: u64 = 7 * 24 * 3600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogIdentity {
    pub workspace: String,
    pub repository: String,
    pub root_run_uuid: String,
    pub parent_run_uuid: Option<String>,
    #[serde(default)]
    pub parent_run_attempt_uuid: Option<String>,
    #[serde(default)]
    pub parent_step_uuid: Option<String>,
    pub run_uuid: String,
    /// Repository-local pipeline build number.
    pub run_number: u64,
    pub run_attempt: u64,
    pub step_uuid: String,
    pub step_name: String,
    pub step_attempt: u64,
    pub execution_path: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogMatch {
    #[serde(flatten)]
    pub identity: LogIdentity,
    /// Absolute byte offset of the match in the original, unsanitized log.
    pub byte_offset: u64,
    pub text: String,
    pub log_complete: bool,
    /// Unix timestamp of the last download.
    pub updated_at: u64,
}

#[derive(Debug, Clone, Default)]
pub struct LogWindow {
    pub lines: Vec<LogLine>,
    pub ranges: Vec<(u64, u64)>,
    pub total: Option<u64>,
    pub complete: bool,
    pub updated_at: u64,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub start: u64,
    pub end: u64,
    pub text: String,
    /// UTF-8 byte spans in sanitized text, using exactly SQLite's FTS5 folding.
    pub highlights: Vec<std::ops::Range<usize>>,
}

/// A validated literal trigram query, escaped once before entering the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogQuery {
    literal: String,
}
impl LogQuery {
    pub fn new(query: &str) -> Result<Self> {
        validate_query(query)?;
        Ok(Self {
            literal: format!("\"{}\"", query.replace('"', "\"\"")),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum EntityKind {
    Workspaces,
    Projects,
    Repositories,
    Runs,
    Steps,
    EarlierSteps,
    Children,
}
impl EntityKind {
    fn name(self) -> &'static str {
        match self {
            Self::Workspaces => "workspaces",
            Self::Projects => "projects",
            Self::Repositories => "repositories",
            Self::Runs => "runs",
            Self::Steps => "steps",
            Self::EarlierSteps => "earlier_steps",
            Self::Children => "children",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedPage<T> {
    pub values: Vec<T>,
    pub next: Option<String>,
    pub updated_at: u64,
}

type Work = Box<dyn FnOnce(&mut Database) + Send>;
struct Job {
    id: u64,
    work: Work,
}

#[derive(Clone)]
pub struct Cache {
    sender: mpsc::Sender<Job>,
    scope: Arc<str>,
    active: Arc<Mutex<u64>>,
    sequence: Arc<AtomicU64>,
    interrupt: Arc<rusqlite::InterruptHandle>,
}
impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache").finish_non_exhaustive()
    }
}

struct Pending {
    id: u64,
    cancelled: Arc<AtomicBool>,
    active: Arc<Mutex<u64>>,
    interrupt: Arc<rusqlite::InterruptHandle>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        // Keep cancellation observable between statements, when sqlite3_interrupt
        // has no effect because no statement is executing yet.
        self.cancelled.store(true, Ordering::Relaxed);
        // Synchronize the check with the worker's handoff: never interrupt the next request.
        let active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *active == self.id {
            self.interrupt.interrupt();
        }
    }
}

impl Cache {
    pub async fn for_app(app: &crate::app::App<'_>) -> Result<Self> {
        let root = if std::env::var_os("BB_CONFIG_DIR").is_some() {
            app.store.directory().join("cache")
        } else {
            directories::ProjectDirs::from("", "", "bb")
                .context("cannot locate cache directory")?
                .cache_dir()
                .to_owned()
        };
        let mut hash = Sha256::new();
        hash.update(app.store.directory().to_string_lossy().as_bytes());
        hash.update(
            std::env::var("BB_API_URL").unwrap_or_else(|_| "https://api.bitbucket.org/2.0/".into()),
        );
        if let Ok(token) = std::env::var("BB_TOKEN") {
            hash.update(b"environment:");
            hash.update(token.as_bytes());
        } else {
            let profile =
                crate::auth::selected_profile(app.cli, &app.config)?.unwrap_or("anonymous");
            hash.update(profile.as_bytes());
            if let Some(user) = app
                .config
                .profiles
                .get(profile)
                .and_then(|p| p.user.as_deref())
            {
                hash.update(user.as_bytes());
            }
        }
        Self::open(
            root.join("logs.sqlite3"),
            hash.finalize()
                .iter()
                .fold(String::with_capacity(64), |mut hex, byte| {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    hex.push(char::from(HEX[usize::from(byte >> 4)]));
                    hex.push(char::from(HEX[usize::from(byte & 15)]));
                    hex
                }),
            QUOTA,
        )
        .await
    }

    pub(crate) async fn open(path: PathBuf, scope: String, quota: u64) -> Result<Self> {
        let (sender, mut receiver) = mpsc::channel::<Job>(16);
        let (ready_tx, ready_rx) = oneshot::channel();
        let active = Arc::new(Mutex::new(0));
        let worker_active = Arc::clone(&active);
        std::thread::Builder::new()
            .name("bb-log-cache".into())
            .spawn(move || {
                let mut database = match Database::open(&path, quota) {
                    Ok(db) => db,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                if ready_tx
                    .send(Ok(database.connection.get_interrupt_handle()))
                    .is_err()
                {
                    return;
                }
                while let Some(job) = receiver.blocking_recv() {
                    *worker_active
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = job.id;
                    (job.work)(&mut database);
                    *worker_active
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = 0;
                }
            })
            .context("cannot start log cache worker")?;
        let interrupt = ready_rx
            .await
            .context("log cache worker stopped during startup")??;
        Ok(Self {
            sender,
            scope: scope.into(),
            active,
            sequence: Arc::new(AtomicU64::new(1)),
            interrupt: Arc::new(interrupt),
        })
    }

    /// Serializes bounded jobs on one SQLite owner thread. Dropping this future
    /// skips queued work or interrupts only its own active statement.
    async fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Database, &str) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (sender, receiver) = oneshot::channel();
        let scope = Arc::clone(&self.scope);
        let id = self.sequence.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        self.sender
            .send(Job {
                id,
                work: Box::new(move |db| {
                    if !sender.is_closed() {
                        let result = (|| {
                            db.connection.progress_handler(
                                1000,
                                Some(move || worker_cancelled.load(Ordering::Relaxed)),
                            )?;
                            let result = work(db, &scope);
                            db.connection.progress_handler(0, None::<fn() -> bool>)?;
                            result
                        })();
                        let _ = sender.send(result);
                    }
                }),
            })
            .await
            .map_err(|error| anyhow::anyhow!("log cache worker stopped: {error}"))?;
        let pending = Pending {
            id,
            cancelled,
            active: Arc::clone(&self.active),
            interrupt: Arc::clone(&self.interrupt),
        };
        let result = receiver.await.context("log cache request interrupted")?;
        drop(pending);
        result
    }

    pub async fn save<T: Serialize + Clone + Send + 'static>(
        &self,
        kind: EntityKind,
        key: String,
        values: &[T],
        next: Option<String>,
    ) -> Result<()> {
        let values = values.to_vec();
        self.call(move |db, scope| {
            let payload = serde_json::to_string(&CachedPage {
                values,
                next,
                updated_at: now(),
            })?;
            db.save(scope, kind.name(), &key, &payload)
        })
        .await
    }

    pub async fn load<T: DeserializeOwned + Send + 'static>(
        &self,
        kind: EntityKind,
        key: String,
    ) -> Result<Option<CachedPage<T>>> {
        self.call(move |db, scope| {
            db.load(scope, kind.name(), &key)?
                .map(|value| serde_json::from_str(&value).map_err(Into::into))
                .transpose()
        })
        .await
    }

    pub async fn restart(&self, identity: LogIdentity) -> Result<()> {
        self.call(move |db, scope| db.restart(scope, &identity))
            .await
    }

    pub async fn index_catalog(
        &self,
        workspace: String,
        items: Vec<crate::discovery::CatalogItem>,
        updated: u64,
    ) -> Result<()> {
        self.call(move |db, scope| db.index_catalog(scope, &workspace, &items, updated))
            .await
    }

    pub async fn find_catalog(
        &self,
        query: crate::discovery::CatalogQuery,
    ) -> Result<CachedPage<crate::discovery::CatalogItem>> {
        self.call(move |db, scope| {
            db.seed_catalog(scope, &query.workspace)?;
            db.find_catalog(scope, &query)
        })
        .await
    }

    pub async fn put(
        &self,
        identity: LogIdentity,
        offset: u64,
        bytes: Vec<u8>,
        total: Option<u64>,
    ) -> Result<()> {
        self.call(move |db, scope| db.put(scope, &identity, offset, &bytes, total))
            .await
    }

    pub async fn window(&self, identity: LogIdentity, from: Option<u64>) -> Result<LogWindow> {
        self.window_matching(identity, from, None).await
    }

    /// Computes highlights on the database worker, never during terminal rendering.
    pub async fn window_matching(
        &self,
        identity: LogIdentity,
        from: Option<u64>,
        query: Option<LogQuery>,
    ) -> Result<LogWindow> {
        self.call(move |db, scope| db.window(scope, &identity, from, query.as_ref()))
            .await
    }

    pub async fn search(
        &self,
        workspace: String,
        repository: Option<String>,
        query: String,
        limit: u32,
    ) -> Result<Vec<LogMatch>> {
        self.search_step(workspace, repository, None, query, limit)
            .await
    }

    pub async fn search_step(
        &self,
        workspace: String,
        repository: Option<String>,
        step: Option<LogIdentity>,
        query: String,
        limit: u32,
    ) -> Result<Vec<LogMatch>> {
        let query = LogQuery::new(&query)?;
        self.call(move |db, scope| {
            db.search(
                scope,
                &workspace,
                repository.as_deref(),
                step.as_ref(),
                &query,
                limit,
            )
        })
        .await
    }

    pub async fn clear(&self, workspace: String) -> Result<usize> {
        self.call(move |db, scope| db.clear(scope, &workspace))
            .await
    }
}

pub fn validate_query(query: &str) -> Result<()> {
    if !(3..=1024).contains(&query.chars().count()) || query.contains(['\0', '\n', '\r']) {
        bail!("search text must contain 3 to 1024 characters on one line");
    }
    Ok(())
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn clean_inline(value: &str) -> String {
    text::clean(value.as_bytes()).replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests;
