use bb_api::{
    Workspace,
    models::{Repository, Run, WorkspaceAccess},
};
use std::collections::HashMap;

use super::{
    browse::{Browse, FindPhase, FindSource, FindTarget, ProjectEntry, ProjectScope},
    service::{Request, Resource, Response, Snapshot},
    tree::{Node, Tree},
};
use crate::log_cache::{BLOCK_SIZE, LogIdentity, LogMatch, LogWindow, clean_inline};
use crate::pipeline_status;

#[cfg(test)]
mod benchmarks {
    use super::*;

    #[test]
    #[ignore = "manual release-mode measurement; run with --ignored --nocapture"]
    fn benchmark_panel_merge() {
        let values: Vec<_> = (0..10_000).map(|i| format!("repository-{i:05}")).collect();
        for _ in 0..3 {
            let snapshot = Snapshot {
                page: crate::log_cache::CachedPage {
                    values: values.clone(),
                    next: None,
                    updated_at: 0,
                },
                cached: false,
                warning: None,
            };
            let mut panel = Panel::default();
            let started = std::time::Instant::now();
            panel.apply(snapshot, false, String::as_str);
            eprintln!(
                "panel_merge items=10000 elapsed_us={}",
                started.elapsed().as_micros()
            );
            assert_eq!(panel.items, values);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Explore,
    Inspect,
}

/// The focused pane. Explore and inspector panes never mix, so focus also determines the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Projects,
    Repositories,
    Runs,
    Builds,
    Tree,
    Logs,
}
impl Focus {
    pub fn screen(self) -> Screen {
        match self {
            Self::Projects | Self::Repositories | Self::Runs => Screen::Explore,
            Self::Builds | Self::Tree | Self::Logs => Screen::Inspect,
        }
    }
}

/// The status line. Its tone belongs to the message, so a new message never inherits an old one's.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum Notice {
    #[default]
    None,
    Info(String),
    Error(String),
}
impl Notice {
    pub fn info(message: impl Into<String>) -> Self {
        Self::Info(message.into())
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error(message.into())
    }
    pub fn text(&self) -> &str {
        match self {
            Self::None => "",
            Self::Info(text) | Self::Error(text) => text,
        }
    }
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }
}

/// How the inspector picks its tree row as execution data arrives.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum TreeSelection {
    /// Expand failures and select the deepest failed step of the inspected run.
    #[default]
    Automatic,
    /// The user chose a row; arriving data keeps it.
    Manual,
    /// Select this step once its row loads, then become manual. Its logs stay open meanwhile.
    Seek { run: String, step: String },
}
impl TreeSelection {
    /// Tree interaction ends automatic selection without abandoning a pending seek.
    pub fn stop_automatic(&mut self) {
        if *self == Self::Automatic {
            *self = Self::Manual;
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Failed,
    Running,
}
impl Filter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Failed => "Failed",
            Self::Running => "Running",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Failed,
            Self::Failed => Self::Running,
            Self::Running => Self::All,
        }
    }
}

#[derive(Debug)]
pub struct Panel<T> {
    pub items: Vec<T>,
    pub selected: usize,
    pub next: Option<String>,
    pub updated: u64,
    pub cached: bool,
}
impl<T> Default for Panel<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            next: None,
            updated: 0,
            cached: false,
        }
    }
}
impl<T> Panel<T> {
    pub fn apply(
        &mut self,
        snapshot: Snapshot<T>,
        append: bool,
        key: impl Fn(&T) -> &str,
    ) -> Option<String> {
        let selected = self
            .items
            .get(self.selected)
            .map(|item| key(item).to_owned());
        if !append {
            self.items.clear();
        }
        // Index once: preserve display order and let the last duplicate replace its row.
        let mut indices: HashMap<String, usize> = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| (key(item).to_owned(), index))
            .collect();
        for value in snapshot.page.values {
            let next = self.items.len();
            let index = *indices.entry(key(&value).to_owned()).or_insert(next);
            if index == next {
                self.items.push(value);
            } else {
                self.items[index] = value;
            }
        }
        self.selected = selected
            .and_then(|selected| self.items.iter().position(|item| key(item) == selected))
            .unwrap_or(self.selected)
            .min(self.items.len().saturating_sub(1));
        self.next = snapshot.page.next;
        self.updated = snapshot.page.updated_at;
        self.cached = snapshot.cached;
        snapshot.warning
    }
}

/// Whether a terminal status still requires a download started after that status.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TailState {
    #[default]
    Idle,
    Running,
    FinalRead,
}

#[derive(Debug, Default)]
pub struct Logs {
    pub identity: Option<LogIdentity>,
    pub window: LogWindow,
    pub scroll: usize,
    pub horizontal: u16,
    pub follow: bool,
    pub zoom: bool,
    pub incremental: bool,
    pub highlight: String,
    pub tail: TailState,
    pub positions: HashMap<String, u64>,
    pub hits: Vec<u64>,
    pub hit: usize,
}

impl Logs {
    pub fn final_tail_pending(&self) -> bool {
        self.tail == TailState::FinalRead
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    Step,
    Repository,
    Workspace,
}
impl SearchScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Step => "Current step",
            Self::Repository => "Repository cache",
            Self::Workspace => "Workspace cache",
        }
    }
}

#[derive(Debug, Default)]
pub enum Modal {
    #[default]
    None,
    Help,
    QuickOpen,
    Workspace {
        input: String,
        selected: usize,
    },
    ChooseWorkspace {
        workspace: String,
        save: bool,
    },
    Search {
        input: String,
        scope: SearchScope,
        matches: Vec<LogMatch>,
        selected: usize,
        editing: bool,
    },
}

/// The inspector has its own execution and reading state above the browser.
#[derive(Debug, Default)]
pub struct Inspector {
    pub tree: Tree,
    pub tree_selected: usize,
    pub selection: TreeSelection,
    pub logs: Logs,
    pub inspection_target: String,
    pub ancestors: std::collections::HashSet<String>,
}

#[derive(Debug)]
pub struct State {
    pub workspace: String,
    pub origin: String,
    pub default_workspace: Option<String>,
    pub offline: bool,
    pub focus: Focus,
    pub filter: Filter,
    pub workspaces: Panel<WorkspaceAccess>,
    pub projects: Panel<ProjectEntry>,
    pub repositories: Panel<Repository>,
    pub runs: Panel<Run>,
    pub runs_loading: bool,
    pub repository: Option<String>,
    pub project: ProjectScope,
    pub inspector: Inspector,
    pub modal: Modal,
    pub notice: Notice,
    pub epoch: u64,
    pub log_revision: u64,
    pub search_revision: u64,
    pub preview: Option<super::navigation::Preview>,
    pub browse: Browse,
}

impl State {
    pub fn new(
        workspace: Option<Workspace>,
        origin: String,
        default_workspace: Option<String>,
        offline: bool,
    ) -> Self {
        Self {
            workspace: workspace.map_or_else(String::new, |workspace| workspace.to_string()),
            origin,
            default_workspace,
            offline,
            focus: Focus::Projects,
            filter: Filter::All,
            workspaces: Panel::default(),
            projects: Panel {
                items: vec![ProjectEntry::Favorites, ProjectEntry::All],
                selected: 1,
                ..Panel::default()
            },
            repositories: Panel::default(),
            runs: Panel::default(),
            runs_loading: false,
            repository: None,
            project: ProjectScope::All,
            inspector: Inspector::default(),
            modal: Modal::None,
            notice: Notice::None,
            epoch: 0,
            log_revision: 0,
            search_revision: 0,
            preview: None,
            browse: Browse::default(),
        }
    }

    pub fn screen(&self) -> Screen {
        self.focus.screen()
    }

    pub fn start(&mut self, repository: Option<String>) -> Vec<Request> {
        if self.workspace.is_empty() {
            self.modal = Modal::Workspace {
                input: String::new(),
                selected: 0,
            };
            return vec![Request::page(Resource::Workspaces)];
        }
        let mut requests = self.reload_find(FindTarget::Projects);
        if let Some(repository) = repository {
            self.runs_loading = true;
            self.repository = Some(repository.clone());
            self.focus = Focus::Runs;
            requests.extend(self.reload_find(FindTarget::Repositories));
            requests.push(Request::page(Resource::Runs(repository)));
        } else if !self.favorite_repositories().is_empty() {
            self.projects.selected = 0;
            requests.extend(self.open_project(&ProjectEntry::Favorites));
        }
        requests
    }

    pub fn choose_workspace(&mut self, workspace: String, save: bool) -> Vec<Request> {
        self.preview = None;
        self.epoch += 1;
        self.workspace.clone_from(&workspace);
        self.origin = if save { "default" } else { "session" }.into();
        self.repository = None;
        self.project = ProjectScope::All;
        self.browse.reset();
        self.projects = Panel {
            items: vec![ProjectEntry::Favorites, ProjectEntry::All],
            selected: 1,
            ..Panel::default()
        };
        self.repositories = Panel::default();
        self.runs = Panel::default();
        self.runs_loading = false;
        self.inspector = Inspector::default();
        self.filter = Filter::All;
        self.focus = Focus::Projects;
        self.modal = Modal::None;
        let mut requests = self.start(None);
        if save {
            requests.push(Request::SaveWorkspace(workspace));
        }
        requests
    }

    pub fn inspect(&mut self, run: Run) -> Vec<Request> {
        let Some(repository) = self.repository.clone() else {
            return Vec::new();
        };
        self.epoch += 1;
        self.focus = Focus::Tree;
        self.preview = None;
        self.inspector.selection = TreeSelection::Automatic;
        self.inspector.inspection_target.clone_from(&run.uuid);
        self.inspector.ancestors.clear();
        self.inspector.ancestors.insert(run.uuid.clone());
        let parent = run
            .trigger
            .metadata
            .pipeline_uuid
            .as_ref()
            .map(|parent| parent.uuid.clone());
        self.inspector.tree = Tree::new(run);
        if let Some(parent) = parent {
            self.inspector.ancestors.insert(parent.clone());
            self.inspector.tree.expanded.insert(parent.clone());
            self.inspector.tree.root = parent;
        }
        self.inspector
            .tree
            .expand_to(&self.inspector.inspection_target);
        self.inspector.tree_selected = 0;
        self.remember_log();
        let positions = std::mem::take(&mut self.inspector.logs.positions);
        self.inspector.logs = Logs {
            positions,
            follow: true,
            incremental: true,
            ..Logs::default()
        };
        vec![
            Request::page(Resource::Run {
                repository: repository.clone(),
                uuid: self.inspector.tree.root.clone(),
            }),
            Request::page(Resource::Steps {
                repository: repository.clone(),
                run: self.inspector.tree.root.clone(),
            }),
            Request::page(Resource::Children(repository)),
        ]
    }

    pub fn filtered_runs(&self) -> Vec<usize> {
        self.runs
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, run)| {
                (match self.filter {
                    Filter::All => true,
                    Filter::Failed => pipeline_status::failed(&run.state),
                    Filter::Running => pipeline_status::running(&run.state),
                })
                .then_some(index)
            })
            .collect()
    }

    pub fn workspace_choices(&self, input: &str) -> Vec<&WorkspaceAccess> {
        let input = input.to_lowercase();
        self.workspaces
            .items
            .iter()
            .filter(|access| {
                access.workspace.slug.to_lowercase().contains(&input)
                    || access.workspace.name.to_lowercase().contains(&input)
            })
            .collect()
    }

    fn accepts_repository(&self, request: &Request) -> bool {
        if let Request::Page { resource, .. } = request {
            let repository = match resource {
                Resource::Runs(repository)
                | Resource::Children(repository)
                | Resource::Run { repository, .. }
                | Resource::Steps { repository, .. }
                | Resource::Earlier { repository, .. } => Some(repository),
                Resource::Workspaces => None,
            };
            if repository.is_some_and(|repository| self.repository.as_ref() != Some(repository)) {
                return false;
            }
        }
        true
    }

    pub fn accept(&mut self, request: &Request, response: Response) -> Vec<Request> {
        if !self.accepts_repository(request) {
            return Vec::new();
        }
        let append = matches!(request, Request::Page { next: Some(_), .. });
        let mut requests = Vec::new();
        let warning = match response {
            Response::Workspaces(snapshot) => self
                .workspaces
                .apply(snapshot, append, |access| &access.workspace.slug),
            Response::Catalog(snapshot) => self.accept_catalog(request, snapshot),
            Response::FavoritesSaved {
                repositories,
                repository,
                enabled,
            } => {
                self.browse.favorites = repositories;
                self.browse
                    .pending_favorites
                    .remove(&repository.to_string());
                if self.project == ProjectScope::Favorites {
                    self.show_favorites();
                }
                if matches!(self.modal, Modal::QuickOpen)
                    && self.browse.quick.finder.text.is_empty()
                {
                    self.quick_favorites();
                }
                self.notice = Notice::Info(format!(
                    "{} favorites: {}",
                    if enabled { "Added to" } else { "Removed from" },
                    clean_inline(&repository.to_string())
                ));
                None
            }
            Response::Runs(snapshot) => self.accept_runs(request, snapshot, append, &mut requests),
            Response::Steps(snapshot) => {
                let warning = snapshot.warning;
                if let Request::Page {
                    resource: Resource::Steps { run, .. },
                    ..
                } = request
                {
                    self.inspector
                        .tree
                        .steps
                        .insert(run.clone(), snapshot.page.values);
                } else if let Request::Page {
                    resource: Resource::Earlier { run, step, .. },
                    ..
                } = request
                    && let Some(value) = snapshot.page.values.into_iter().next()
                {
                    self.inspector
                        .tree
                        .earlier
                        .insert((run.clone(), step.clone()), value);
                }
                requests.extend(self.update_tree());
                warning
            }
            Response::Log {
                window,
                from,
                incremental,
                finished,
            } => {
                self.accept_log(request, window, from, incremental, finished);
                None
            }
            Response::Search(results) => {
                requests.extend(self.accept_search(request, results));
                None
            }
            Response::WorkspaceSaved(workspace) => {
                self.default_workspace = Some(workspace.clone());
                self.notice = Notice::Info(format!(
                    "Default workspace saved: {}",
                    clean_inline(&workspace)
                ));
                None
            }
            Response::Auth { .. } => None,
        };
        if let Some(warning) = warning {
            self.notice = Notice::Error(clean_inline(&warning));
        }
        self.schedule_preview();
        requests
    }

    /// Ends whatever was waiting for a failed request and reports its error.
    pub fn fail(&mut self, request: &Request, error: &str) {
        match request {
            Request::SaveFavorite { repository, .. } => {
                self.browse
                    .pending_favorites
                    .remove(&repository.to_string());
            }
            Request::Find {
                target,
                source,
                next,
                ..
            } if self.find_is_current(request) => {
                let finder = self.browse.finder_mut(*target);
                finder.phase = match (source, next, finder.phase) {
                    (FindSource::Network, None, _) => FindPhase::Failed,
                    (FindSource::Cache, _, FindPhase::Cache) => FindPhase::Idle,
                    // A failed later page, or a cache miss while Bitbucket is still answering,
                    // leaves the revision's outcome to the request that is still running.
                    (_, _, phase) => phase,
                };
            }
            Request::Page {
                resource: Resource::Runs(_),
                ..
            } => self.runs_loading = false,
            _ => {}
        }
        self.notice = Notice::Error(clean_inline(error));
    }

    /// Match the complete search identity before touching either modal or log state.
    pub fn search_is_current(&self, request: &Request) -> bool {
        let (
            Modal::Search { input, scope, .. },
            Request::Search {
                workspace,
                repository,
                step,
                query,
                revision,
                scope: requested_scope,
            },
        ) = (&self.modal, request)
        else {
            return false;
        };
        *revision == self.search_revision
            && scope == requested_scope
            && input == query
            && workspace == &self.workspace
            && repository.as_ref()
                == if *scope == SearchScope::Workspace {
                    None
                } else {
                    self.repository.as_ref()
                }
            && match (step, &self.inspector.logs.identity) {
                (Some(step), Some(current)) if *scope == SearchScope::Step => {
                    same_log(step, current)
                }
                (None, _) => *scope != SearchScope::Step,
                _ => false,
            }
    }

    pub fn log_is_current(&self, request: &Request) -> bool {
        matches!(request, Request::Log { identity, revision, .. }
            if *revision == self.log_revision && self.inspector.logs.identity.as_ref().is_some_and(|current| same_log(current, identity)))
    }

    pub fn log_query(&self) -> Option<crate::log_cache::LogQuery> {
        crate::log_cache::LogQuery::new(&self.inspector.logs.highlight).ok()
    }

    fn accept_search(&mut self, request: &Request, results: Vec<LogMatch>) -> Vec<Request> {
        if !self.search_is_current(request) {
            return Vec::new();
        }
        let mut requests = Vec::new();
        if let Request::Search {
            step: Some(step),
            query,
            ..
        } = request
            && self
                .inspector
                .logs
                .identity
                .as_ref()
                .is_some_and(|current| same_log(current, step))
        {
            self.inspector.logs.hits = results.iter().map(|hit| hit.byte_offset).collect();
            self.inspector.logs.hit = 0;
            self.inspector.logs.highlight.clone_from(query);
            self.log_revision += 1;
            requests.push(Request::Log {
                identity: step.clone(),
                range: None,
                from: self
                    .inspector
                    .logs
                    .window
                    .lines
                    .get(self.inspector.logs.scroll)
                    .map(|line| line.start),
                query: self.log_query(),
                revision: self.log_revision,
            });
        }
        if let (
            Modal::Search {
                input,
                matches,
                selected,
                editing,
                ..
            },
            Request::Search { query, .. },
        ) = (&mut self.modal, request)
            && input == query
        {
            *matches = results;
            *selected = 0;
            *editing = false;
        }
        requests
    }

    fn accept_runs(
        &mut self,
        request: &Request,
        snapshot: Snapshot<Run>,
        append: bool,
        requests: &mut Vec<Request>,
    ) -> Option<String> {
        if matches!(
            request,
            Request::Page {
                resource: Resource::Runs(_),
                ..
            }
        ) {
            self.runs_loading = false;
            let selected = self
                .filtered_runs()
                .get(self.runs.selected)
                .and_then(|index| self.runs.items.get(*index))
                .map(|run| run.uuid.clone());
            let warning = self.runs.apply(snapshot, append, |run| &run.uuid);
            self.runs.items.sort_by(|left, right| {
                right
                    .created_on
                    .cmp(&left.created_on)
                    .then(right.build_number.cmp(&left.build_number))
            });
            let indices = self.filtered_runs();
            self.runs.selected = selected
                .and_then(|uuid| {
                    indices
                        .iter()
                        .position(|index| self.runs.items[*index].uuid == uuid)
                })
                .unwrap_or(self.runs.selected)
                .min(indices.len().saturating_sub(1));
            return warning;
        }
        let warning = snapshot.warning;
        if matches!(
            request,
            Request::Page {
                resource: Resource::Children(_),
                ..
            }
        ) {
            self.inspector.tree.children_loaded = snapshot.page.updated_at != 0;
        }
        for run in snapshot.page.values {
            if run.uuid == self.inspector.inspection_target
                && !self.runs.items.iter().any(|item| item.uuid == run.uuid)
            {
                self.runs.items.push(run.clone());
                self.runs.items.sort_by(|left, right| {
                    right
                        .created_on
                        .cmp(&left.created_on)
                        .then(right.build_number.cmp(&left.build_number))
                });
                if let Some(index) = self
                    .filtered_runs()
                    .iter()
                    .position(|index| self.runs.items[*index].uuid == run.uuid)
                {
                    self.runs.selected = index;
                }
            }
            if run.uuid == self.inspector.tree.root
                && let Some(parent) = &run.trigger.metadata.pipeline_uuid
            {
                if !self.inspector.ancestors.insert(parent.uuid.clone())
                    || self.inspector.ancestors.len() > 64
                {
                    self.notice =
                        Notice::error("Invalid parent cycle or excessive pipeline nesting");
                    self.inspector.tree.runs.insert(run.uuid.clone(), run);
                    continue;
                }
                self.inspector.tree.root.clone_from(&parent.uuid);
                self.inspector.tree.expanded.insert(parent.uuid.clone());
                if let Some(repository) = &self.repository {
                    requests.push(Request::page(Resource::Run {
                        repository: repository.clone(),
                        uuid: parent.uuid.clone(),
                    }));
                }
            }
            if self
                .inspector
                .tree
                .runs
                .get(&run.uuid)
                .is_some_and(|old| old.run_number != run.run_number)
            {
                self.inspector.tree.steps.remove(&run.uuid);
            }
            self.inspector.tree.runs.insert(run.uuid.clone(), run);
        }
        requests.extend(self.update_tree());
        warning
    }

    pub fn update_tree(&mut self) -> Vec<Request> {
        let Some(repository) = self.repository.clone() else {
            return Vec::new();
        };
        let selected = self
            .inspector
            .tree
            .rows
            .get(self.inspector.tree_selected)
            .map(|row| row.key.clone());
        match &self.inspector.selection {
            TreeSelection::Automatic => {
                self.inspector.tree.expand_failures();
                self.inspector
                    .tree
                    .expand_to(&self.inspector.inspection_target);
            }
            TreeSelection::Seek { run, .. } => self.inspector.tree.expand_to(run),
            TreeSelection::Manual => {}
        }
        self.inspector.tree.rebuild();
        self.inspector.tree_selected = selected
            .and_then(|key| {
                self.inspector
                    .tree
                    .rows
                    .iter()
                    .position(|row| row.key == key)
            })
            .unwrap_or(self.inspector.tree_selected)
            .min(self.inspector.tree.rows.len().saturating_sub(1));
        let mut requests = Vec::new();
        for row in &self.inspector.tree.rows {
            match &row.node {
                Node::Run(run) if row.open && !self.inspector.tree.steps.contains_key(run) => {
                    requests.push(Request::page(Resource::Steps {
                        repository: repository.clone(),
                        run: run.clone(),
                    }));
                }
                Node::Step { run, step } if self.inspector.tree.step(run, step).is_none() => {
                    requests.push(Request::page(Resource::Earlier {
                        repository: repository.clone(),
                        run: run.clone(),
                        step: step.clone(),
                    }));
                }
                _ => {}
            }
        }
        if self.inspector.selection == TreeSelection::Automatic
            && self.inspector.tree.children_loaded
            && requests.is_empty()
        {
            let failure = self
                .inspector
                .tree
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.failed && self.in_inspection(&row.node) && !row.expandable)
                .max_by_key(|(index, row)| (row.depth, std::cmp::Reverse(*index)))
                .map(|(index, _)| index);
            if let Some(index) = failure {
                self.inspector.tree_selected = index;
            } else if let Some(index) = self
                .inspector
                .tree
                .rows
                .iter()
                .position(|row| self.in_inspection(&row.node) && !row.expandable)
            {
                self.inspector.tree_selected = index;
            }
        }
        if let TreeSelection::Seek { run, step } = &self.inspector.selection
            && let Some(index) = self.inspector.tree.rows.iter().position(|row| matches!(&row.node, Node::Step { run: parent, step: uuid } if parent == run && uuid == step))
        {
            self.inspector.tree_selected = index;
            self.inspector.selection = TreeSelection::Manual;
        }
        requests.extend(self.select_log());
        requests
    }

    fn in_inspection(&self, node: &Node) -> bool {
        let Node::Step { run, .. } = node else {
            return false;
        };
        self.inspector
            .tree
            .descends_from(&self.inspector.inspection_target, run)
    }

    pub fn select_log(&mut self) -> Vec<Request> {
        let identity = self.repository.as_ref().and_then(|repository| {
            self.inspector
                .tree
                .rows
                .get(self.inspector.tree_selected)
                .filter(|row| !row.expandable)
                .and_then(|row| self.inspector.tree.identity(repository, &row.node))
        });
        let Some(identity) = identity else {
            let seeking = matches!(self.inspector.selection, TreeSelection::Seek { .. });
            if !seeking && !self.inspector.tree.rows.is_empty() {
                self.remember_log();
                let positions = std::mem::take(&mut self.inspector.logs.positions);
                self.inspector.logs = Logs {
                    positions,
                    ..Logs::default()
                };
            }
            return Vec::new();
        };
        let running = self
            .inspector
            .tree
            .step(&identity.run_uuid, &identity.step_uuid)
            .is_some_and(|step| pipeline_status::running(&step.state));
        if self
            .inspector
            .logs
            .identity
            .as_ref()
            .is_some_and(|old| same_log(old, &identity))
        {
            if self.inspector.logs.tail == TailState::Running && !running {
                // Invalidate in-flight downloads started before terminal status was observed.
                self.inspector.logs.tail = TailState::FinalRead;
                self.log_revision += 1;
            }
            if running {
                self.inspector.logs.tail = TailState::Running;
            }
            return if self.inspector.logs.final_tail_pending() {
                self.poll_log()
            } else {
                Vec::new()
            };
        }
        if let Some(old) = &self.inspector.logs.identity
            && let Some(line) = self
                .inspector
                .logs
                .window
                .lines
                .get(self.inspector.logs.scroll)
        {
            self.inspector
                .logs
                .positions
                .insert(log_key(old), line.start);
        }
        let from = self
            .inspector
            .logs
            .positions
            .get(&log_key(&identity))
            .copied();
        self.log_revision += 1;
        self.inspector.logs.tail = if running {
            TailState::Running
        } else {
            TailState::Idle
        };
        self.inspector.logs.identity = Some(identity.clone());
        self.inspector.logs.window = LogWindow::default();
        self.inspector.logs.hits.clear();
        self.inspector.logs.hit = 0;
        self.inspector.logs.scroll = 0;
        self.inspector.logs.follow = from.is_none();
        self.inspector.logs.incremental = true;
        self.inspector.logs.horizontal = 0;
        vec![Request::Log {
            identity,
            range: (!self.offline).then_some(bb_api::ByteRange::Suffix(BLOCK_SIZE as u64)),
            from,
            query: self.log_query(),
            revision: self.log_revision,
        }]
    }

    /// Only a completed download from the current revision satisfies the final tail read.
    pub fn accept_log(
        &mut self,
        request: &Request,
        window: LogWindow,
        from: Option<u64>,
        incremental: bool,
        finished: bool,
    ) {
        if !self.log_is_current(request) {
            return;
        }
        if finished
            && self.inspector.logs.final_tail_pending()
            && matches!(
                request,
                Request::Log {
                    range: Some(_),
                    from: None,
                    ..
                }
            )
        {
            self.inspector.logs.tail = TailState::Idle;
        }
        let anchor = from.or_else(|| {
            self.inspector
                .logs
                .window
                .lines
                .get(self.inspector.logs.scroll)
                .map(|line| line.start)
        });
        self.inspector.logs.scroll = if self.inspector.logs.follow {
            window.lines.len().saturating_sub(1)
        } else {
            anchor
                .and_then(|offset| window.lines.iter().position(|line| line.end > offset))
                .unwrap_or(0)
        };
        self.inspector.logs.window = window;
        self.inspector.logs.incremental &= incremental;
    }

    pub fn poll_status(&self) -> Vec<Request> {
        if self.offline || self.screen() != Screen::Inspect {
            return Vec::new();
        }
        let Some(repository) = &self.repository else {
            return Vec::new();
        };
        let mut requests = vec![Request::page(Resource::Run {
            repository: repository.clone(),
            uuid: self.inspector.tree.root.clone(),
        })];
        for run in self.inspector.tree.runs.values().filter(|run| {
            pipeline_status::running(&run.state)
                || self
                    .inspector
                    .tree
                    .steps
                    .get(&run.uuid)
                    .is_some_and(|steps| {
                        steps
                            .iter()
                            .any(|step| pipeline_status::running(&step.state))
                    })
        }) {
            if self.inspector.tree.expanded.contains(&run.uuid) {
                requests.push(Request::page(Resource::Steps {
                    repository: repository.clone(),
                    run: run.uuid.clone(),
                }));
                if run.uuid != self.inspector.tree.root {
                    requests.push(Request::page(Resource::Run {
                        repository: repository.clone(),
                        uuid: run.uuid.clone(),
                    }));
                }
            }
        }
        if self
            .inspector
            .tree
            .runs
            .get(&self.inspector.tree.root)
            .is_some_and(|run| pipeline_status::running(&run.state))
        {
            requests.push(Request::page(Resource::Children(repository.clone())));
        }
        requests
    }

    pub fn poll_log(&self) -> Vec<Request> {
        if self.offline
            || !self.inspector.logs.follow
            || (!self.inspector.logs.incremental && !self.inspector.logs.final_tail_pending())
            || self.screen() != Screen::Inspect
        {
            return Vec::new();
        }
        let Some(identity) = &self.inspector.logs.identity else {
            return Vec::new();
        };
        if !self.inspector.logs.final_tail_pending()
            && !self
                .inspector
                .tree
                .step(&identity.run_uuid, &identity.step_uuid)
                .is_some_and(|step| pipeline_status::running(&step.state))
        {
            return Vec::new();
        }
        let range = self
            .inspector
            .logs
            .window
            .ranges
            .last()
            .map_or(bb_api::ByteRange::Suffix(BLOCK_SIZE as u64), |(_, end)| {
                bb_api::ByteRange::From(end.saturating_sub(4096))
            });
        vec![Request::Log {
            identity: identity.clone(),
            range: Some(range),
            from: None,
            query: self.log_query(),
            revision: self.log_revision,
        }]
    }
}

pub fn same_log(left: &LogIdentity, right: &LogIdentity) -> bool {
    left.repository == right.repository
        && left.run_uuid == right.run_uuid
        && left.run_attempt == right.run_attempt
        && left.step_uuid == right.step_uuid
        && left.step_attempt == right.step_attempt
}

pub(super) fn log_key(identity: &LogIdentity) -> String {
    format!(
        "{}:{}:{}:{}",
        identity.run_uuid, identity.run_attempt, identity.step_uuid, identity.step_attempt
    )
}
