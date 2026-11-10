//! Repository bookmarks and cancellable catalog navigation.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use bb_api::{
    RepoRef,
    models::{Project, Repository},
};
use crossterm::event::{KeyCode, KeyEvent};

use super::{
    keys::{edit, move_index},
    service::{Request, Resource, Snapshot},
    state::{Focus, Modal, Notice, Panel, State},
};
use crate::{
    discovery::{CatalogItem, CatalogKind, CatalogQuery},
    log_cache::CachedPage,
};

#[derive(Debug, Clone)]
pub enum ProjectEntry {
    Favorites,
    All,
    Project(Project),
}
impl ProjectEntry {
    pub fn key(&self) -> &str {
        match self {
            Self::Favorites => "@favorites",
            Self::All => "@all",
            Self::Project(project) => &project.key,
        }
    }

    pub fn scope(&self) -> ProjectScope {
        match self {
            Self::Favorites => ProjectScope::Favorites,
            Self::All => ProjectScope::All,
            Self::Project(project) => ProjectScope::Project(project.key.clone()),
        }
    }
}

/// Which repositories the middle pane lists; opening a `ProjectEntry` selects one.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum ProjectScope {
    #[default]
    All,
    Favorites,
    Project(String),
}
impl ProjectScope {
    /// The `ProjectEntry::key` of the row that selects this scope.
    pub fn key(&self) -> &str {
        match self {
            Self::Favorites => "@favorites",
            Self::All => "@all",
            Self::Project(key) => key,
        }
    }

    /// The project key a repository search is restricted to.
    pub fn project(&self) -> Option<&str> {
        match self {
            Self::Project(key) => Some(key),
            Self::All | Self::Favorites => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FindTarget {
    Projects,
    Repositories,
    Quick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindSource {
    Cache,
    Network,
}

#[derive(Debug, Default)]
pub struct Finder {
    pub text: String,
    pub revision: u64,
    pub phase: FindPhase,
}
impl Finder {
    pub fn loading(&self) -> bool {
        matches!(self.phase, FindPhase::Cache | FindPhase::Network { .. })
    }
}

/// Progress of the current finder revision. A cache snapshot can fill rows first;
/// once Bitbucket answers, later cache snapshots for that revision are stale.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FindPhase {
    /// Nothing is pending, or the rows are local only (favorites, offline cache).
    #[default]
    Idle,
    /// Offline: waiting for the cache snapshot.
    Cache,
    /// Waiting for Bitbucket. `due` delays the request while the user is typing.
    Network { due: Option<Instant> },
    /// Bitbucket answered this revision.
    Loaded,
    /// Bitbucket failed; cached rows, if any, stay visible.
    Failed,
}

#[derive(Debug)]
pub struct QuickOpen {
    pub kind: CatalogKind,
    pub finder: Finder,
    pub results: Panel<CatalogItem>,
}
impl Default for QuickOpen {
    fn default() -> Self {
        Self {
            kind: CatalogKind::Repositories,
            finder: Finder::default(),
            results: Panel::default(),
        }
    }
}

#[derive(Debug, Default)]
pub struct Browse {
    pub api_origin: String,
    pub favorites: Vec<RepoRef>,
    pub favorite_metadata: HashMap<String, Repository>,
    /// Repositories whose favorite toggle is being saved; repeated toggles wait for it.
    pub pending_favorites: HashSet<String>,
    pub projects: Finder,
    pub repositories: Finder,
    pub quick: QuickOpen,
    pub editing: Option<FindTarget>,
}

impl Browse {
    pub fn reset(&mut self) {
        self.projects = Finder::default();
        self.repositories = Finder::default();
        self.quick = QuickOpen::default();
        self.editing = None;
    }

    pub fn finder(&self, target: FindTarget) -> &Finder {
        match target {
            FindTarget::Projects => &self.projects,
            FindTarget::Repositories => &self.repositories,
            FindTarget::Quick => &self.quick.finder,
        }
    }

    pub fn finder_mut(&mut self, target: FindTarget) -> &mut Finder {
        match target {
            FindTarget::Projects => &mut self.projects,
            FindTarget::Repositories => &mut self.repositories,
            FindTarget::Quick => &mut self.quick.finder,
        }
    }
}

impl State {
    pub fn favorite_repositories(&self) -> Vec<RepoRef> {
        self.browse
            .favorites
            .iter()
            .filter(|repository| repository.workspace().as_str() == self.workspace)
            .cloned()
            .collect()
    }

    pub fn is_favorite(&self, repository: &str) -> bool {
        self.browse
            .favorites
            .iter()
            .any(|favorite| favorite.to_string() == repository)
    }

    fn favorite_items(&self) -> Vec<CatalogItem> {
        let mut items: Vec<_> =
            self.favorite_repositories()
                .into_iter()
                .map(|reference| {
                    let repository =
                        self.browse
                            .favorite_metadata
                            .get(&reference.to_string())
                            .or_else(|| {
                                self.repositories.items.iter().find(|repository| {
                                    repository.full_name == reference.to_string()
                                })
                            })
                            .cloned()
                            .unwrap_or_else(|| Repository {
                                name: reference.slug().into(),
                                slug: reference.slug().into(),
                                full_name: reference.to_string(),
                                ..Repository::default()
                            });
                    CatalogItem::Repository(Box::new(repository))
                })
                .collect();
        items.sort_unstable_by_key(|item| (item.name().to_lowercase(), item.key().to_owned()));
        items
    }

    pub fn show_favorites(&mut self) {
        let text = self.browse.repositories.text.trim().to_lowercase();
        let values = self
            .favorite_items()
            .into_iter()
            .filter(|item| item.search_text().contains(&text))
            .filter_map(|item| match item {
                CatalogItem::Repository(repository) => Some(*repository),
                CatalogItem::Project(_) => None,
            })
            .collect();
        self.repositories.apply(
            Snapshot {
                page: CachedPage {
                    values,
                    next: None,
                    updated_at: 0,
                },
                cached: false,
                warning: None,
            },
            false,
            |repository| &repository.full_name,
        );
    }

    pub fn quick_favorites(&mut self) {
        self.browse.quick.results.apply(
            Snapshot {
                page: CachedPage {
                    values: self.favorite_items(),
                    next: None,
                    updated_at: 0,
                },
                cached: false,
                warning: None,
            },
            false,
            CatalogItem::key,
        );
    }

    pub fn toggle_favorite(&mut self, reference: Option<String>) -> Vec<Request> {
        let reference = reference.or_else(|| {
            if self.focus == Focus::Repositories {
                self.repositories
                    .items
                    .get(self.repositories.selected)
                    .map(|repository| repository.full_name.clone())
            } else if self.focus != Focus::Projects {
                self.repository.clone()
            } else {
                None
            }
        });
        let Some(reference) = reference else {
            return Vec::new();
        };
        if self.browse.pending_favorites.contains(&reference) {
            return Vec::new();
        }
        let repository = match reference.parse::<RepoRef>() {
            Ok(repository) => repository,
            Err(error) => {
                self.notice = Notice::error(error.to_string());
                return Vec::new();
            }
        };
        let enabled = !self.is_favorite(&reference);
        self.browse.pending_favorites.insert(reference);
        self.notice = Notice::info("Saving favorite…");
        vec![Request::SaveFavorite {
            origin: self.browse.api_origin.clone(),
            repository,
            enabled,
        }]
    }

    pub fn catalog_query(&self, target: FindTarget) -> CatalogQuery {
        CatalogQuery {
            workspace: self.workspace.clone(),
            kind: match target {
                FindTarget::Projects => CatalogKind::Projects,
                FindTarget::Repositories => CatalogKind::Repositories,
                FindTarget::Quick => self.browse.quick.kind,
            },
            project: (target == FindTarget::Repositories)
                .then(|| self.project.project().map(str::to_owned))
                .flatten(),
            text: self.browse.finder(target).text.clone(),
        }
    }

    fn find_request(
        &self,
        target: FindTarget,
        source: FindSource,
        next: Option<String>,
    ) -> Request {
        Request::Find {
            query: self.catalog_query(target),
            target,
            revision: self.browse.finder(target).revision,
            source,
            next,
        }
    }

    pub fn reload_find(&mut self, target: FindTarget) -> Vec<Request> {
        self.begin_find(target, false)
    }

    /// Whether a target lists local favorites instead of searching Bitbucket.
    fn finds_favorites(&self, target: FindTarget) -> bool {
        match target {
            FindTarget::Repositories => self.project == ProjectScope::Favorites,
            FindTarget::Quick => {
                self.browse.quick.kind == CatalogKind::Repositories
                    && self.browse.quick.finder.text.trim().is_empty()
            }
            FindTarget::Projects => false,
        }
    }

    fn list_favorites(&mut self, target: FindTarget) {
        match target {
            FindTarget::Repositories => self.show_favorites(),
            FindTarget::Quick => self.quick_favorites(),
            FindTarget::Projects => {}
        }
    }

    fn begin_find(&mut self, target: FindTarget, debounce: bool) -> Vec<Request> {
        let favorites = self.finds_favorites(target);
        let offline = self.offline;
        let finder = self.browse.finder_mut(target);
        finder.revision += 1;
        finder.phase = if favorites {
            FindPhase::Idle
        } else if offline {
            FindPhase::Cache
        } else {
            FindPhase::Network {
                due: debounce.then(|| Instant::now() + Duration::from_millis(250)),
            }
        };
        if favorites {
            // The cache request still refreshes favorite metadata.
            self.list_favorites(target);
            return vec![self.find_request(target, FindSource::Cache, None)];
        }
        let mut requests = vec![self.find_request(target, FindSource::Cache, None)];
        if !offline && !debounce {
            requests.push(self.find_request(target, FindSource::Network, None));
        }
        requests
    }

    pub fn find_due(&mut self, now: Instant) -> Vec<Request> {
        let mut requests = Vec::new();
        for target in [
            FindTarget::Projects,
            FindTarget::Repositories,
            FindTarget::Quick,
        ] {
            let FindPhase::Network { due: Some(due) } = self.browse.finder(target).phase else {
                continue;
            };
            if due > now {
                continue;
            }
            if target != FindTarget::Quick || matches!(self.modal, Modal::QuickOpen) {
                self.browse.finder_mut(target).phase = FindPhase::Network { due: None };
                requests.push(self.find_request(target, FindSource::Network, None));
            } else {
                self.browse.finder_mut(target).phase = FindPhase::Idle;
            }
        }
        requests
    }

    pub fn find_is_current(&self, request: &Request) -> bool {
        let Request::Find {
            query,
            target,
            revision,
            source,
            ..
        } = request
        else {
            return true;
        };
        self.browse.finder(*target).revision == *revision
            && *query == self.catalog_query(*target)
            && (*target != FindTarget::Quick || matches!(self.modal, Modal::QuickOpen))
            && (*target != FindTarget::Repositories
                || self.project != ProjectScope::Favorites
                || *source == FindSource::Cache)
    }

    pub fn accept_catalog(
        &mut self,
        request: &Request,
        snapshot: Snapshot<CatalogItem>,
    ) -> Option<String> {
        if !self.find_is_current(request) {
            return None;
        }
        let Request::Find {
            target,
            source,
            next,
            ..
        } = request
        else {
            return None;
        };
        let favorites = self.finds_favorites(*target);
        let finder = self.browse.finder_mut(*target);
        finder.phase = match (*source, finder.phase) {
            (FindSource::Cache, FindPhase::Loaded) => return None,
            (FindSource::Network, _) => FindPhase::Loaded,
            (FindSource::Cache, FindPhase::Cache) => FindPhase::Idle,
            (FindSource::Cache, phase) => phase,
        };
        if favorites {
            finder.phase = FindPhase::Idle;
        }
        for item in &snapshot.page.values {
            if let CatalogItem::Repository(repository) = item
                && self.is_favorite(&repository.full_name)
            {
                self.browse
                    .favorite_metadata
                    .insert(repository.full_name.clone(), *repository.clone());
            }
        }
        if favorites {
            self.list_favorites(*target);
            return snapshot.warning;
        }
        self.apply_catalog(*target, snapshot, next.is_some())
    }

    fn apply_catalog(
        &mut self,
        target: FindTarget,
        snapshot: Snapshot<CatalogItem>,
        append: bool,
    ) -> Option<String> {
        let warning = snapshot.warning;
        let cached = snapshot.cached;
        let next = snapshot.page.next;
        let updated_at = snapshot.page.updated_at;
        match target {
            FindTarget::Quick => {
                let mut values = snapshot.page.values;
                if cached && self.browse.quick.kind == CatalogKind::Repositories {
                    let text = self.browse.quick.finder.text.trim().to_lowercase();
                    let favorites = self.favorite_items();
                    for favorite in favorites {
                        if favorite.search_text().contains(&text)
                            && !values.iter().any(|item| item.key() == favorite.key())
                        {
                            values.push(favorite);
                        }
                    }
                    values.sort_unstable_by_key(|item| {
                        (item.name().to_lowercase(), item.key().to_owned())
                    });
                }
                self.browse.quick.results.apply(
                    Snapshot {
                        page: CachedPage {
                            values,
                            next,
                            updated_at,
                        },
                        cached,
                        warning: None,
                    },
                    append,
                    CatalogItem::key,
                );
            }
            FindTarget::Projects => {
                let mut values = Vec::new();
                if !append && self.browse.projects.text.trim().is_empty() {
                    values.extend([ProjectEntry::Favorites, ProjectEntry::All]);
                }
                values.extend(
                    snapshot
                        .page
                        .values
                        .into_iter()
                        .filter_map(|item| match item {
                            CatalogItem::Project(project) => Some(ProjectEntry::Project(project)),
                            CatalogItem::Repository(_) => None,
                        }),
                );
                self.projects.apply(
                    Snapshot {
                        page: CachedPage {
                            values,
                            next,
                            updated_at,
                        },
                        cached,
                        warning: None,
                    },
                    append,
                    ProjectEntry::key,
                );
            }
            FindTarget::Repositories => {
                let values = snapshot
                    .page
                    .values
                    .into_iter()
                    .filter_map(|item| match item {
                        CatalogItem::Repository(repository) => Some(*repository),
                        CatalogItem::Project(_) => None,
                    })
                    .collect();
                self.repositories.apply(
                    Snapshot {
                        page: CachedPage {
                            values,
                            next,
                            updated_at,
                        },
                        cached,
                        warning: None,
                    },
                    append,
                    |repository| &repository.full_name,
                );
            }
        }
        warning
    }

    pub fn open_project(&mut self, project: &ProjectEntry) -> Vec<Request> {
        self.preview = None;
        if project.key() == self.project.key() && self.browse.repositories.revision > 0 {
            self.focus = Focus::Repositories;
            return Vec::new();
        }
        self.epoch += 1;
        self.project = project.scope();
        self.browse.repositories = Finder {
            revision: self.browse.repositories.revision + 1,
            ..Finder::default()
        };
        self.repository = None;
        self.repositories = Panel::default();
        self.runs = Panel::default();
        self.runs_loading = false;
        self.focus = Focus::Repositories;
        self.reload_find(FindTarget::Repositories)
    }

    pub fn open_repository(&mut self, repository: String) -> Vec<Request> {
        self.preview = None;
        if self.repository.as_ref() == Some(&repository) {
            self.focus = Focus::Runs;
            return Vec::new();
        }
        self.repository = Some(repository.clone());
        self.runs = Panel::default();
        self.runs_loading = true;
        self.epoch += 1;
        self.focus = Focus::Runs;
        vec![Request::page(Resource::Runs(repository))]
    }

    pub fn open_quick(&mut self) -> Vec<Request> {
        if self.workspace.is_empty() {
            self.notice = Notice::info("Choose a workspace before searching");
            return self.start(None);
        }
        self.notice = Notice::None;
        self.browse.editing = None;
        self.modal = Modal::QuickOpen;
        self.browse.quick = QuickOpen::default();
        self.reload_find(FindTarget::Quick)
    }

    pub fn quick_key(&mut self, key: KeyEvent) -> Vec<Request> {
        match key.code {
            KeyCode::Esc => {
                self.modal = Modal::None;
                self.browse.quick.finder.phase = FindPhase::Idle;
                Vec::new()
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.browse.quick.kind = match self.browse.quick.kind {
                    CatalogKind::Repositories => CatalogKind::Projects,
                    CatalogKind::Projects => CatalogKind::Repositories,
                };
                self.browse.quick.results = Panel::default();
                self.reload_find(FindTarget::Quick)
            }
            KeyCode::Up | KeyCode::Down => {
                let results = &mut self.browse.quick.results;
                results.selected = move_index(
                    results.selected,
                    key.code == KeyCode::Up,
                    1,
                    results.items.len(),
                );
                Vec::new()
            }
            KeyCode::PageDown => self.more_find(FindTarget::Quick),
            KeyCode::Char('*') => {
                let reference = self
                    .browse
                    .quick
                    .results
                    .items
                    .get(self.browse.quick.results.selected)
                    .and_then(|item| match item {
                        CatalogItem::Repository(repository) => Some(repository.full_name.clone()),
                        CatalogItem::Project(_) => None,
                    });
                reference.map_or_else(Vec::new, |reference| self.toggle_favorite(Some(reference)))
            }
            KeyCode::Enter => {
                let Some(item) = self
                    .browse
                    .quick
                    .results
                    .items
                    .get(self.browse.quick.results.selected)
                    .cloned()
                else {
                    return Vec::new();
                };
                self.modal = Modal::None;
                self.browse.quick.finder.phase = FindPhase::Idle;
                match item {
                    CatalogItem::Project(project) => {
                        self.open_project(&ProjectEntry::Project(project))
                    }
                    CatalogItem::Repository(repository) => {
                        self.project = repository
                            .project
                            .as_ref()
                            .map_or(ProjectScope::All, |project| {
                                ProjectScope::Project(project.key.clone())
                            });
                        self.browse.repositories = Finder::default();
                        self.repositories = Panel {
                            items: vec![*repository.clone()],
                            ..Panel::default()
                        };
                        let mut requests = self.open_repository(repository.full_name.clone());
                        requests.extend(self.reload_find(FindTarget::Repositories));
                        requests
                    }
                }
            }
            _ => {
                let before = self.browse.quick.finder.text.clone();
                edit(&mut self.browse.quick.finder.text, key);
                if before == self.browse.quick.finder.text {
                    return Vec::new();
                }
                self.notice = Notice::None;
                self.browse.quick.results = Panel::default();
                self.begin_find(FindTarget::Quick, true)
            }
        }
    }

    pub fn filter_key(&mut self, key: KeyEvent) -> Vec<Request> {
        let Some(target) = self.browse.editing else {
            return Vec::new();
        };
        if key.code == KeyCode::Enter {
            self.browse.editing = None;
            return Vec::new();
        }
        let before = self.browse.finder(target).text.clone();
        if key.code == KeyCode::Esc {
            self.browse.finder_mut(target).text.clear();
            self.browse.editing = None;
        } else {
            edit(&mut self.browse.finder_mut(target).text, key);
        }
        if before == self.browse.finder(target).text {
            return Vec::new();
        }
        // Do not let Enter act on a row from the previous query while results arrive.
        match target {
            FindTarget::Projects => self.projects = Panel::default(),
            FindTarget::Repositories => self.repositories = Panel::default(),
            FindTarget::Quick => {}
        }
        self.begin_find(target, key.code != KeyCode::Esc)
    }

    pub fn more_find(&mut self, target: FindTarget) -> Vec<Request> {
        if self.offline {
            self.notice = Notice::info("Cached results only");
            return Vec::new();
        }
        let panel = match target {
            FindTarget::Projects => (self.projects.next.clone(), self.projects.items.len()),
            FindTarget::Repositories => (
                self.repositories.next.clone(),
                self.repositories.items.len(),
            ),
            FindTarget::Quick => (
                self.browse.quick.results.next.clone(),
                self.browse.quick.results.items.len(),
            ),
        };
        if panel.1 >= 10_000 {
            self.notice = Notice::info("10,000 results loaded; refine the search");
            return Vec::new();
        }
        panel.0.map_or_else(Vec::new, |next| {
            vec![self.find_request(target, FindSource::Network, Some(next))]
        })
    }
}
