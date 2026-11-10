//! Cancellable I/O jobs. Terminal state never holds a database connection or HTTP body.

use anyhow::{Context, Result, bail, ensure};
use bb_api::{
    ByteRange, Client, Method,
    models::{CollectionPage, Project, Repository, Run, Step, WorkspaceAccess},
};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashSet;
use tokio::sync::mpsc;

use super::browse::{FindSource, FindTarget};
use crate::{
    app::{App, encode},
    discovery::{CatalogItem, CatalogKind, CatalogQuery},
    log_cache::{
        BLOCK_SIZE, Cache, CachedPage, EntityKind, LogIdentity, LogMatch, LogQuery, LogWindow, now,
    },
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Resource {
    Workspaces,
    Runs(String),
    Run {
        repository: String,
        uuid: String,
    },
    Steps {
        repository: String,
        run: String,
    },
    Earlier {
        repository: String,
        run: String,
        step: String,
    },
    Children(String),
}

impl Resource {
    fn descriptor(&self) -> (EntityKind, String, String, Vec<(String, String)>) {
        let mut query = vec![("pagelen".into(), "50".into())];
        let (kind, key, path) = match self {
            Self::Workspaces => (
                EntityKind::Workspaces,
                "accessible".into(),
                "user/workspaces".into(),
            ),
            Self::Runs(repository) => {
                query.push(("sort".into(), "-created_on".into()));
                (
                    EntityKind::Runs,
                    repository.clone(),
                    format!("{}/pipelines", repo_path(repository)),
                )
            }
            Self::Run { repository, uuid } => (
                EntityKind::Runs,
                format!("{repository}:{uuid}"),
                run_path(repository, uuid),
            ),
            Self::Steps { repository, run } => (
                EntityKind::Steps,
                format!("{repository}:{run}"),
                format!("{}/steps", run_path(repository, run)),
            ),
            Self::Earlier {
                repository,
                run,
                step,
            } => (
                EntityKind::EarlierSteps,
                format!("{repository}:{run}:{step}"),
                format!("{}/steps/{}", run_path(repository, run), encode(step)),
            ),
            Self::Children(repository) => {
                query.extend([
                    ("sort".into(), "-created_on".into()),
                    ("trigger_type".into(), "PARENT_STEP".into()),
                ]);
                (
                    EntityKind::Children,
                    repository.clone(),
                    format!("{}/pipelines", repo_path(repository)),
                )
            }
        };
        (kind, key, path, query)
    }
}

fn repo_path(repository: &str) -> String {
    format!(
        "repositories/{}",
        repository
            .split('/')
            .map(encode)
            .collect::<Vec<_>>()
            .join("/")
    )
}

fn run_path(repository: &str, run: &str) -> String {
    format!("{}/pipelines/{}", repo_path(repository), encode(run))
}

#[derive(Debug, Clone)]
pub enum Request {
    Find {
        query: CatalogQuery,
        target: FindTarget,
        revision: u64,
        source: FindSource,
        next: Option<String>,
    },
    SaveFavorite {
        origin: String,
        repository: bb_api::RepoRef,
        enabled: bool,
    },
    Page {
        resource: Resource,
        next: Option<String>,
    },
    Log {
        identity: LogIdentity,
        range: Option<ByteRange>,
        from: Option<u64>,
        query: Option<LogQuery>,
        revision: u64,
    },
    Search {
        workspace: String,
        repository: Option<String>,
        step: Option<LogIdentity>,
        query: String,
        revision: u64,
        scope: super::state::SearchScope,
    },
    SaveWorkspace(String),
    RefreshAuth,
}

impl Request {
    pub fn persistent(&self) -> bool {
        matches!(self, Self::SaveFavorite { .. } | Self::SaveWorkspace(_))
    }
    pub fn page(resource: Resource) -> Self {
        Self::Page {
            resource,
            next: None,
        }
    }

    pub fn key(&self) -> String {
        match self {
            Self::Find {
                query,
                target,
                revision,
                source,
                next,
            } => format!("find:{target:?}:{revision}:{source:?}:{query:?}:{next:?}"),
            Self::SaveFavorite {
                origin,
                repository,
                enabled,
            } => format!("favorite:{origin}:{repository}:{enabled}"),
            Self::Page { resource, next } => format!("{resource:?}:{next:?}"),
            Self::Log {
                identity, revision, ..
            } => format!(
                "log:{}:{}:{}:{}:{}:{revision}",
                identity.repository,
                identity.run_uuid,
                identity.run_attempt,
                identity.step_uuid,
                identity.step_attempt
            ),
            Self::Search { revision, .. } => format!("search:{revision}"),
            Self::SaveWorkspace(workspace) => format!("default:{workspace}"),
            Self::RefreshAuth => "auth".into(),
        }
    }
}

#[derive(Debug)]
pub struct Snapshot<T> {
    pub page: CachedPage<T>,
    pub cached: bool,
    pub warning: Option<String>,
}

#[derive(Debug)]
pub enum Response {
    Workspaces(Snapshot<WorkspaceAccess>),
    Catalog(Snapshot<CatalogItem>),
    FavoritesSaved {
        repositories: Vec<bb_api::RepoRef>,
        repository: bb_api::RepoRef,
        enabled: bool,
    },
    Runs(Snapshot<Run>),
    Steps(Snapshot<Step>),
    Log {
        window: LogWindow,
        from: Option<u64>,
        incremental: bool,
        finished: bool,
    },
    Search(Vec<LogMatch>),
    WorkspaceSaved(String),
    Auth {
        client: Client,
        expires: Option<u64>,
    },
}

#[derive(Debug)]
pub struct Progress {
    pub epoch: u64,
    pub ticket: u64,
    pub response: Response,
}

pub async fn execute(
    app: &App<'_>,
    cache: &Cache,
    client: Option<&Client>,
    request: &Request,
    epoch: u64,
    ticket: u64,
    progress: &mpsc::Sender<Progress>,
) -> Result<Response> {
    preview_page(cache, client, request, epoch, ticket, progress).await?;
    execute_request(app, cache, client, request, epoch, ticket, progress).await
}

async fn preview_page(
    cache: &Cache,
    client: Option<&Client>,
    request: &Request,
    epoch: u64,
    ticket: u64,
    progress: &mpsc::Sender<Progress>,
) -> Result<()> {
    // Show an existing snapshot before awaiting HTTP. The scheduler's ticket and
    // navigation epoch protect these intermediate results just like final ones.
    if client.is_some()
        && let Request::Page {
            resource,
            next: None,
        } = request
        && let Some(response) = cached_response(cache, resource).await?
    {
        progress
            .send(Progress {
                epoch,
                ticket,
                response,
            })
            .await
            .context("terminal closed")?;
    }
    Ok(())
}

async fn execute_request(
    app: &App<'_>,
    cache: &Cache,
    client: Option<&Client>,
    request: &Request,
    epoch: u64,
    ticket: u64,
    progress: &mpsc::Sender<Progress>,
) -> Result<Response> {
    match request {
        Request::Find {
            query,
            source,
            next,
            ..
        } => Ok(Response::Catalog(
            find_catalog(cache, client, query, *source, next.as_deref()).await?,
        )),
        Request::SaveFavorite {
            origin,
            repository,
            enabled,
        } => {
            let repositories =
                crate::favorites::set(&app.store, origin, repository, *enabled).await?;
            Ok(Response::FavoritesSaved {
                repositories,
                repository: repository.clone(),
                enabled: *enabled,
            })
        }
        Request::Page { resource, next } => match resource {
            Resource::Workspaces => Ok(Response::Workspaces(
                read(cache, client, resource, next.as_deref()).await?,
            )),
            Resource::Run { .. } => Ok(Response::Runs(read_run(cache, client, resource).await?)),
            Resource::Runs(_) | Resource::Children(_) => Ok(Response::Runs(
                read(cache, client, resource, next.as_deref()).await?,
            )),
            Resource::Steps { .. } | Resource::Earlier { .. } => Ok(Response::Steps(
                read(cache, client, resource, next.as_deref()).await?,
            )),
        },
        Request::Log {
            identity,
            range,
            from,
            query,
            ..
        } => {
            read_log(
                cache,
                client,
                identity,
                *range,
                LogProgress {
                    epoch,
                    ticket,
                    sender: progress,
                    query: query.clone(),
                    from: *from,
                },
            )
            .await
        }

        Request::Search {
            workspace,
            repository,
            step,
            query,
            ..
        } => Ok(Response::Search(
            cache
                .search_step(
                    workspace.clone(),
                    repository.clone(),
                    step.clone(),
                    query.clone(),
                    1000,
                )
                .await?,
        )),
        Request::SaveWorkspace(workspace) => save_workspace(app, workspace).await,
        Request::RefreshAuth => {
            let credentials = crate::auth::credential(app.cli, &app.store, &app.config, false)
                .await?
                .context("run bb auth login or set BB_TOKEN")?;
            Ok(Response::Auth {
                client: crate::app::client(app.cli, Some(&credentials))?,
                expires: credentials.expires_at,
            })
        }
    }
}

async fn read_log(
    cache: &Cache,
    client: Option<&Client>,
    identity: &LogIdentity,
    range: Option<ByteRange>,
    progress: LogProgress<'_>,
) -> Result<Response> {
    let cached = cache
        .window_matching(identity.clone(), progress.from, progress.query.clone())
        .await?;
    if !cached.lines.is_empty() {
        progress
            .sender
            .send(Progress {
                epoch: progress.epoch,
                ticket: progress.ticket,
                response: Response::Log {
                    window: cached,
                    from: progress.from,
                    incremental: true,
                    finished: false,
                },
            })
            .await
            .context("terminal closed")?;
    }
    let incremental = if let (Some(client), Some(range)) = (client, range) {
        download(
            cache,
            client,
            identity,
            range,
            LogProgress {
                epoch: progress.epoch,
                ticket: progress.ticket,
                sender: progress.sender,
                query: progress.query.clone(),
                from: progress.from,
            },
        )
        .await?
    } else {
        true
    };
    Ok(Response::Log {
        window: cache
            .window_matching(identity.clone(), progress.from, progress.query)
            .await?,
        from: progress.from,
        incremental,
        finished: true,
    })
}

async fn cached_response(cache: &Cache, resource: &Resource) -> Result<Option<Response>> {
    match resource {
        Resource::Workspaces => {
            let snapshot = read(cache, None, resource, None).await?;
            Ok((snapshot.page.updated_at != 0).then_some(Response::Workspaces(snapshot)))
        }
        Resource::Run { .. } | Resource::Runs(_) | Resource::Children(_) => {
            let snapshot = read_run(cache, None, resource).await?;
            Ok((snapshot.page.updated_at != 0).then_some(Response::Runs(snapshot)))
        }
        Resource::Steps { .. } | Resource::Earlier { .. } => {
            let snapshot = read(cache, None, resource, None).await?;
            Ok((snapshot.page.updated_at != 0).then_some(Response::Steps(snapshot)))
        }
    }
}

async fn save_workspace(app: &App<'_>, workspace: &str) -> Result<Response> {
    let selected = workspace.parse()?;
    app.store
        .update(move |config| {
            config.preferences.default_workspace = Some(selected);
            Ok(())
        })
        .await?;
    Ok(Response::WorkspaceSaved(workspace.into()))
}

async fn find_catalog(
    cache: &Cache,
    client: Option<&Client>,
    query: &CatalogQuery,
    source: FindSource,
    next: Option<&str>,
) -> Result<Snapshot<CatalogItem>> {
    if source == FindSource::Cache {
        return Ok(Snapshot {
            page: cache.find_catalog(query.clone()).await?,
            cached: true,
            warning: None,
        });
    }
    let client = client.context("catalog search requires an online session")?;
    let path = query.path();
    let parameters = if next.is_none() {
        query.parameters()
    } else {
        Vec::new()
    };
    let (values, next) = match query.kind {
        CatalogKind::Repositories => {
            let page = client
                .page::<Repository>(next.unwrap_or(&path), &parameters)
                .await?;
            (
                page.values
                    .into_iter()
                    .map(|repository| CatalogItem::Repository(Box::new(repository)))
                    .collect(),
                page.next,
            )
        }
        CatalogKind::Projects => {
            let page = client
                .page::<Project>(next.unwrap_or(&path), &parameters)
                .await?;
            (
                page.values.into_iter().map(CatalogItem::Project).collect(),
                page.next,
            )
        }
    };
    let page = CachedPage {
        values,
        next,
        updated_at: now(),
    };
    let warning = cache
        .index_catalog(
            query.workspace.clone(),
            page.values.clone(),
            page.updated_at,
        )
        .await
        .err()
        .map(|error| format!("Results loaded; could not update cache: {error:#}"));
    Ok(Snapshot {
        page,
        cached: false,
        warning,
    })
}

async fn read_run(
    cache: &Cache,
    client: Option<&Client>,
    resource: &Resource,
) -> Result<Snapshot<Run>> {
    let mut snapshot = read(cache, client, resource, None).await?;
    if snapshot.page.values.is_empty()
        && let Resource::Run { repository, uuid } = resource
    {
        for kind in [EntityKind::Runs, EntityKind::Children] {
            if let Some(page) = cache.load::<Run>(kind, repository.clone()).await?
                && let Some(run) = page.values.into_iter().find(|run| run.uuid == *uuid)
            {
                snapshot.page = CachedPage {
                    values: vec![run],
                    next: None,
                    updated_at: page.updated_at,
                };
                break;
            }
        }
    }
    Ok(snapshot)
}

async fn read<T: DeserializeOwned + Serialize + Clone + Send + 'static>(
    cache: &Cache,
    client: Option<&Client>,
    resource: &Resource,
    next: Option<&str>,
) -> Result<Snapshot<T>> {
    let (kind, key, path, query) = resource.descriptor();
    let cached = cache.load::<T>(kind, key.clone()).await?;
    let Some(client) = client else {
        return Ok(Snapshot {
            page: cached.unwrap_or(CachedPage {
                values: Vec::new(),
                next: None,
                updated_at: 0,
            }),
            cached: true,
            warning: None,
        });
    };
    let page = match fetch(
        client,
        resource,
        next.unwrap_or(&path),
        if next.is_some() { &[] } else { &query },
    )
    .await
    {
        Ok(page) => page,
        Err(error) => {
            // A next page must never repeat the entire cached first page.
            if next.is_none()
                && let Some(page) = cached
            {
                return Ok(Snapshot {
                    page,
                    cached: true,
                    warning: Some(format!("Using cached data: {error:#}")),
                });
            }
            return Err(error);
        }
    };
    let mut combined = if next.is_some() {
        cached.map_or_else(Vec::new, |page| page.values)
    } else {
        Vec::new()
    };
    combined.extend_from_slice(&page.values);
    ensure!(
        combined.len() <= 10_000,
        "collection cache limit reached (10,000 records)"
    );
    cache.save(kind, key, &combined, page.next.clone()).await?;
    Ok(Snapshot {
        page: CachedPage {
            values: page.values,
            next: page.next,
            updated_at: now(),
        },
        cached: false,
        warning: None,
    })
}

async fn fetch<T: DeserializeOwned>(
    client: &Client,
    resource: &Resource,
    path: &str,
    query: &[(String, String)],
) -> Result<CollectionPage<T>> {
    if matches!(resource, Resource::Run { .. } | Resource::Earlier { .. }) {
        let response = client.raw(Method::Get, path, &[], None, &[]).await?;
        return Ok(CollectionPage {
            values: vec![serde_json::from_slice(&response.body)?],
            next: None,
        });
    }
    let mut page = client.page(path, query).await?;
    if matches!(resource, Resource::Children(_) | Resource::Steps { .. }) {
        let mut seen = HashSet::new();
        while let Some(next) = page.next.take() {
            ensure!(
                seen.insert(next.clone()) && seen.len() <= 1000,
                "invalid pagination cycle"
            );
            let remaining = client.page::<T>(&next, &[]).await?;
            page.values.extend(remaining.values);
            ensure!(
                page.values.len() <= 10_000,
                "pipeline collection exceeds 10,000 records"
            );
            page.next = remaining.next;
        }
    }
    Ok(page)
}

struct LogProgress<'a> {
    epoch: u64,
    ticket: u64,
    sender: &'a mpsc::Sender<Progress>,
    query: Option<LogQuery>,
    from: Option<u64>,
}

/// Coalesces transport chunks into aligned cache blocks. Partial buffers flush
/// every 100 ms; cancellation retains completed writes and drops at most one block.
async fn download(
    cache: &Cache,
    client: &Client,
    identity: &LogIdentity,
    range: ByteRange,
    progress: LogProgress<'_>,
) -> Result<bool> {
    let path = format!(
        "{}/steps/{}/log",
        run_path(&identity.repository, &identity.run_uuid),
        encode(&identity.step_uuid)
    );
    let mut response = client.log_range(&path, range).await?;
    if response.status() == 304 {
        return Ok(true);
    }
    if response.status() == 416 {
        let window = cache.window(identity.clone(), None).await?;
        if response
            .total()
            .is_some_and(|size| window.ranges.last().is_some_and(|(_, end)| *end > size))
        {
            // The remote log was truncated or replaced; start again from the new tail.
            response = client
                .log_range(&path, ByteRange::Suffix(BLOCK_SIZE as u64))
                .await?;
        } else {
            if let Some(total) = response.total() {
                cache
                    .put(identity.clone(), total, Vec::new(), Some(total))
                    .await?;
            }
            return Ok(true);
        }
    }
    if response.status() == 416 && response.total() == Some(0) {
        cache.put(identity.clone(), 0, Vec::new(), Some(0)).await?;
        return Ok(true);
    }
    ensure!(
        matches!(response.status(), 200 | 206),
        "unexpected log response ({})",
        response.status()
    );
    ingest_log(cache, identity, response, progress).await
}

/// Stores bounded blocks and publishes snapshots independently of network framing.
async fn ingest_log(
    cache: &Cache,
    identity: &LogIdentity,
    mut response: bb_api::LogResponse,
    progress: LogProgress<'_>,
) -> Result<bool> {
    let incremental = response.status() == 206;
    if !incremental {
        cache.restart(identity.clone()).await?;
    }
    let interval = response.range();
    let mut position = interval.as_ref().map_or(0, |range| range.start);
    let initial = position;
    let total = response.total();
    let expected = interval.map(|range| range.end).or(total);
    let mut buffer = Vec::with_capacity(BLOCK_SIZE);
    let mut changed = false;
    let mut timer = tokio::time::interval(std::time::Duration::from_millis(100));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    timer.tick().await;
    loop {
        let chunk = tokio::select! {
            chunk = response.chunk() => chunk,
            _ = timer.tick() => {
                if !changed { continue; }
                flush_log(cache, identity, &mut position, &mut buffer, total).await?;
                let window = cache.window_matching(identity.clone(), progress.from, progress.query.clone()).await?;
                progress.sender.send(Progress {
                    epoch: progress.epoch, ticket: progress.ticket,
                    response: Response::Log {
                        window, from: progress.from,
                        incremental, finished: false,
                    },
                }).await.context("terminal closed")?;
                changed = false;
                continue;
            }
        };
        let chunk = match chunk {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                flush_log(cache, identity, &mut position, &mut buffer, total).await?;
                return Err(error.into());
            }
        };
        let mut remaining = chunk.as_ref();
        while !remaining.is_empty() {
            let target = BLOCK_SIZE - usize::try_from(position % BLOCK_SIZE as u64)?;
            let count = remaining.len().min(target - buffer.len());
            let end = position
                .checked_add((buffer.len() + count) as u64)
                .context("log offset overflow")?;
            ensure!(
                expected.is_none_or(|expected| end <= expected),
                "log body exceeds Content-Range"
            );
            if end - initial > 64 * 1024 * 1024 {
                flush_log(cache, identity, &mut position, &mut buffer, total).await?;
                bail!("Download paused at 64 MiB; cached portions remain searchable");
            }
            buffer.extend_from_slice(&remaining[..count]);
            changed = true;
            remaining = &remaining[count..];
            if buffer.len() == target {
                flush_log(cache, identity, &mut position, &mut buffer, total).await?;
            }
        }
    }
    flush_log(cache, identity, &mut position, &mut buffer, total).await?;
    ensure!(
        expected.is_none_or(|end| end == position),
        "incomplete log response; downloaded portions remain cached"
    );
    cache
        .put(
            identity.clone(),
            position,
            Vec::new(),
            total.or((!incremental).then_some(position)),
        )
        .await?;
    Ok(incremental)
}

async fn flush_log(
    cache: &Cache,
    identity: &LogIdentity,
    position: &mut u64,
    buffer: &mut Vec<u8>,
    total: Option<u64>,
) -> Result<()> {
    if buffer.is_empty() {
        return Ok(());
    }
    let length = buffer.len() as u64;
    cache
        .put(identity.clone(), *position, std::mem::take(buffer), total)
        .await?;
    *position += length;
    Ok(())
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
