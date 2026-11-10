use bb_api::ByteRange;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    browse::{FindTarget, ProjectScope},
    service::{Request, Resource},
    state::{Focus, Modal, Notice, Screen, SearchScope, State, TreeSelection},
    tree::{Node, Tree},
};
use crate::log_cache::{BLOCK_SIZE, LogMatch, LogWindow, validate_query};

impl State {
    pub fn key(&mut self, key: KeyEvent, page_size: usize) -> (Option<u8>, Vec<Request>) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return (Some(2), Vec::new());
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p') {
            return (None, self.open_quick());
        }
        if matches!(self.modal, Modal::QuickOpen) {
            let requests = self.quick_key(key);
            self.schedule_preview();
            return (None, requests);
        }
        if !matches!(self.modal, Modal::None) {
            let requests = self.modal_key(key);
            self.schedule_preview();
            return (None, requests);
        }
        if self.browse.editing.is_some() {
            let requests = self.filter_key(key);
            self.schedule_preview();
            return (None, requests);
        }
        if key.code == KeyCode::Char('q') {
            return (Some(0), Vec::new());
        }
        (None, self.normal_key(key, page_size))
    }

    fn normal_key(&mut self, key: KeyEvent, page_size: usize) -> Vec<Request> {
        match key.code {
            KeyCode::Char('?') => {
                self.modal = Modal::Help;
                Vec::new()
            }
            KeyCode::Char('w') => {
                self.modal = Modal::Workspace {
                    input: String::new(),
                    selected: 0,
                };
                vec![Request::page(Resource::Workspaces)]
            }
            KeyCode::Char('/')
                if self.screen() == Screen::Explore
                    && matches!(self.focus, Focus::Projects | Focus::Repositories) =>
            {
                self.browse.editing = Some(if self.focus == Focus::Projects {
                    FindTarget::Projects
                } else {
                    FindTarget::Repositories
                });
                Vec::new()
            }
            KeyCode::Char('/' | 'L') => self.search_logs(),
            KeyCode::Tab => self.focus_pane(false, true),
            KeyCode::BackTab => self.focus_pane(true, true),
            KeyCode::Char('1') => self.focus_number(0),
            KeyCode::Char('2') => self.focus_number(1),
            KeyCode::Char('3') => self.focus_number(2),
            KeyCode::Esc => self.escape(key),
            KeyCode::Char('*') => self.toggle_favorite(None),
            KeyCode::Char('F') => {
                self.filter = self.filter.next();
                self.runs.selected = 0;
                self.schedule_preview();
                Vec::new()
            }
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('m') => self.more(),
            KeyCode::Char('e') => self.failure(false),
            KeyCode::Char('E') => self.failure(true),
            KeyCode::Char('z') if self.screen() == Screen::Inspect => {
                self.inspector.logs.zoom = !self.inspector.logs.zoom;
                self.focus = Focus::Logs;
                Vec::new()
            }
            KeyCode::Char('f') if self.screen() == Screen::Inspect => {
                self.inspector.logs.follow = !self.inspector.logs.follow;
                if self.inspector.logs.follow {
                    self.log_end()
                } else {
                    self.inspector.logs.scroll = self
                        .inspector
                        .logs
                        .window
                        .lines
                        .len()
                        .saturating_sub(page_size);
                    Vec::new()
                }
            }
            KeyCode::Char('n') => self.search_hit(false),
            KeyCode::Char('N') => self.search_hit(true),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(false, 1, page_size),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(true, 1, page_size),
            KeyCode::PageDown => self.move_selection(false, page_size.max(1), page_size),
            KeyCode::PageUp => self.move_selection(true, page_size.max(1), page_size),
            KeyCode::Home if self.focus == Focus::Logs => self.log_start(),
            KeyCode::End if self.focus == Focus::Logs => self.log_end(),
            KeyCode::Home => self.move_selection(true, usize::MAX, page_size),
            KeyCode::End => self.move_selection(false, usize::MAX, page_size),
            KeyCode::Right | KeyCode::Left
                if self.focus == Focus::Logs && key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.inspector.logs.horizontal = if key.code == KeyCode::Right {
                    self.inspector.logs.horizontal.saturating_add(8)
                } else {
                    self.inspector.logs.horizontal.saturating_sub(8)
                };
                Vec::new()
            }
            KeyCode::Enter => self.open(),
            KeyCode::Char(' ') if self.focus == Focus::Tree => self.open(),
            KeyCode::Right | KeyCode::Char('l') => {
                if self.screen() == Screen::Explore {
                    self.open()
                } else {
                    self.focus_pane(false, false)
                }
            }
            KeyCode::Left | KeyCode::Char('h') => self.focus_pane(true, false),
            _ => Vec::new(),
        }
    }

    fn escape(&mut self, key: KeyEvent) -> Vec<Request> {
        let target = match self.focus {
            Focus::Projects => Some(FindTarget::Projects),
            Focus::Repositories => Some(FindTarget::Repositories),
            _ => None,
        };
        if let Some(target) = target
            && !self.browse.finder(target).text.is_empty()
        {
            self.browse.editing = Some(target);
            return self.filter_key(key);
        }
        self.back()
    }

    fn search_logs(&mut self) -> Vec<Request> {
        self.search_revision += 1;
        self.modal = Modal::Search {
            input: String::new(),
            scope: if self.inspector.logs.identity.is_some() && self.screen() == Screen::Inspect {
                SearchScope::Step
            } else if self.repository.is_some() {
                SearchScope::Repository
            } else {
                SearchScope::Workspace
            },
            matches: Vec::new(),
            selected: 0,
            editing: true,
        };
        Vec::new()
    }

    fn back(&mut self) -> Vec<Request> {
        if self.inspector.logs.zoom {
            self.inspector.logs.zoom = false;
        } else if self.screen() == Screen::Inspect {
            self.preview = None;
            if let Some(index) = self
                .filtered_runs()
                .iter()
                .position(|index| self.runs.items[*index].uuid == self.inspector.inspection_target)
            {
                self.runs.selected = index;
            }
            self.remember_log();
            self.epoch += 1;
            self.focus = Focus::Runs;
        } else {
            return self.focus_pane(true, false);
        }
        Vec::new()
    }

    fn move_selection(&mut self, up: bool, amount: usize, page_size: usize) -> Vec<Request> {
        if self.focus == Focus::Logs && self.inspector.logs.follow {
            self.inspector.logs.scroll = self
                .inspector
                .logs
                .window
                .lines
                .len()
                .saturating_sub(page_size);
            self.inspector.logs.follow = false;
        }
        let length = match self.focus {
            Focus::Projects => self.projects.items.len(),
            Focus::Repositories => self.repositories.items.len(),
            Focus::Runs | Focus::Builds => self.filtered_runs().len(),
            Focus::Tree => self.inspector.tree.rows.len(),
            Focus::Logs => self
                .inspector
                .logs
                .window
                .lines
                .len()
                .saturating_sub(page_size)
                .saturating_add(1),
        };
        let selected = match self.focus {
            Focus::Projects => &mut self.projects.selected,
            Focus::Repositories => &mut self.repositories.selected,
            Focus::Runs | Focus::Builds => &mut self.runs.selected,
            Focus::Tree => &mut self.inspector.tree_selected,
            Focus::Logs => &mut self.inspector.logs.scroll,
        };
        let before = *selected;
        *selected = move_index(*selected, up, amount, length);
        if self.focus == Focus::Logs {
            self.inspector.logs.follow = false;
            if up && *selected == 0 {
                return self.log_older();
            }
            if !up && *selected == before {
                return self.log_newer();
            }
        }
        if self.focus == Focus::Tree {
            self.inspector.selection = TreeSelection::Manual;
            return self.select_log();
        }
        if !up && before == *selected {
            return self.more();
        }
        self.schedule_preview();
        Vec::new()
    }

    fn open(&mut self) -> Vec<Request> {
        match self.focus {
            Focus::Projects => {
                let Some(project) = self.projects.items.get(self.projects.selected).cloned() else {
                    return Vec::new();
                };
                let mut requests = self.open_project(&project);
                self.schedule_preview();
                requests.extend(self.flush_preview());
                requests
            }
            Focus::Repositories => {
                let Some(repository) = self.repositories.items.get(self.repositories.selected)
                else {
                    return Vec::new();
                };
                self.open_repository(repository.full_name.clone())
            }
            Focus::Runs => {
                let Some(run) = self
                    .filtered_runs()
                    .get(self.runs.selected)
                    .and_then(|index| self.runs.items.get(*index))
                    .cloned()
                else {
                    return self.more();
                };
                self.inspect(run)
            }
            Focus::Builds => self.focus_pane(false, false),
            Focus::Tree => {
                self.inspector.selection.stop_automatic();
                let Some(row) = self.inspector.tree.rows.get(self.inspector.tree_selected) else {
                    return Vec::new();
                };
                if row.expandable {
                    if row.open {
                        self.inspector.tree.expanded.remove(&row.key);
                    } else {
                        self.inspector.tree.expanded.insert(row.key.clone());
                    }
                    self.update_tree()
                } else {
                    self.focus = Focus::Logs;
                    self.select_log()
                }
            }
            Focus::Logs => Vec::new(),
        }
    }

    fn more(&mut self) -> Vec<Request> {
        if self.focus == Focus::Projects {
            return self.more_find(FindTarget::Projects);
        }
        if self.focus == Focus::Repositories {
            return self.more_find(FindTarget::Repositories);
        }
        if self.offline {
            self.notice = Notice::info("Offline: only downloaded pages are available");
            return Vec::new();
        }
        let (resource, next) = match self.focus {
            Focus::Runs | Focus::Builds => {
                let Some(repository) = &self.repository else {
                    return Vec::new();
                };
                (Resource::Runs(repository.clone()), self.runs.next.clone())
            }
            _ => return Vec::new(),
        };
        next.map_or_else(Vec::new, |next| {
            vec![Request::Page {
                resource,
                next: Some(next),
            }]
        })
    }

    fn refresh(&mut self) -> Vec<Request> {
        self.notice = Notice::None;
        if self.screen() == Screen::Explore || self.focus == Focus::Builds {
            return match self.focus {
                Focus::Projects => self.reload_find(FindTarget::Projects),
                Focus::Repositories => self.reload_find(FindTarget::Repositories),
                _ => self
                    .repository
                    .as_ref()
                    .map_or_else(Vec::new, |repository| {
                        vec![Request::page(Resource::Runs(repository.clone()))]
                    }),
            };
        }
        let mut requests = self.poll_status();
        if let Some(repository) = &self.repository {
            requests.extend([
                Request::page(Resource::Children(repository.clone())),
                Request::page(Resource::Steps {
                    repository: repository.clone(),
                    run: self.inspector.tree.root.clone(),
                }),
            ]);
        }
        if let Some(identity) = &self.inspector.logs.identity {
            requests.insert(
                0,
                Request::Log {
                    query: self.log_query(),
                    revision: self.log_revision,
                    identity: identity.clone(),
                    range: (!self.offline).then_some(ByteRange::Suffix(BLOCK_SIZE as u64)),
                    from: None,
                },
            );
        }
        requests
    }

    fn failure(&mut self, reverse: bool) -> Vec<Request> {
        if self.screen() != Screen::Inspect {
            return Vec::new();
        }
        self.inspector.tree.expand_failures();
        self.inspector.selection.stop_automatic();
        let mut requests = self.update_tree();
        let failures: Vec<_> = self
            .inspector
            .tree
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.failed && matches!(row.node, Node::Step { .. }) && !row.expandable
            })
            .map(|(index, _)| index)
            .collect();
        let index = if reverse {
            failures
                .iter()
                .rfind(|index| **index < self.inspector.tree_selected)
                .or_else(|| failures.last())
        } else {
            failures
                .iter()
                .find(|index| **index > self.inspector.tree_selected)
                .or_else(|| failures.first())
        };
        if let Some(index) = index {
            self.inspector.tree_selected = *index;
            self.focus = Focus::Tree;
            requests.extend(self.select_log());
        }
        requests
    }

    fn log_older(&self) -> Vec<Request> {
        let Some(first) = self.inspector.logs.window.lines.first() else {
            return Vec::new();
        };
        if first.start == 0 {
            return Vec::new();
        }
        let from = first.start.saturating_sub(BLOCK_SIZE as u64);
        self.log_range(from, first.start - 1)
    }

    fn log_newer(&self) -> Vec<Request> {
        let Some(last) = self.inspector.logs.window.lines.last() else {
            return Vec::new();
        };
        if self
            .inspector
            .logs
            .window
            .total
            .is_some_and(|total| last.end >= total)
        {
            return Vec::new();
        }
        self.log_range(last.end, last.end.saturating_add(BLOCK_SIZE as u64 - 1))
    }

    fn log_range(&self, start: u64, end: u64) -> Vec<Request> {
        let Some(identity) = &self.inspector.logs.identity else {
            return Vec::new();
        };
        let cached = self
            .inspector
            .logs
            .window
            .ranges
            .iter()
            .any(|(left, right)| *left <= start && *right > start);
        let end = self
            .inspector
            .logs
            .window
            .total
            .map_or(end, |total| end.min(total.saturating_sub(1)));
        vec![Request::Log {
            query: self.log_query(),
            revision: self.log_revision,
            identity: identity.clone(),
            range: (!cached && !self.offline).then_some(ByteRange::Inclusive { start, end }),
            from: Some(start),
        }]
    }

    fn log_start(&mut self) -> Vec<Request> {
        self.inspector.logs.follow = false;
        self.log_range(0, BLOCK_SIZE as u64 - 1)
    }

    fn log_end(&mut self) -> Vec<Request> {
        self.inspector.logs.follow = true;
        self.inspector
            .logs
            .identity
            .as_ref()
            .map_or_else(Vec::new, |identity| {
                vec![Request::Log {
                    query: self.log_query(),
                    revision: self.log_revision,
                    identity: identity.clone(),
                    range: (!self.offline
                        && (self.inspector.logs.incremental
                            || self.inspector.logs.final_tail_pending()))
                    .then_some(ByteRange::Suffix(BLOCK_SIZE as u64)),
                    from: None,
                }]
            })
    }

    fn search_hit(&mut self, reverse: bool) -> Vec<Request> {
        if self.screen() != Screen::Inspect || self.inspector.logs.hits.is_empty() {
            return Vec::new();
        }
        let count = self.inspector.logs.hits.len();
        self.inspector.logs.hit =
            (self.inspector.logs.hit + if reverse { count - 1 } else { 1 }) % count;
        self.inspector.logs.follow = false;
        self.focus = Focus::Logs;
        let from = self.inspector.logs.hits[self.inspector.logs.hit];
        self.inspector
            .logs
            .identity
            .as_ref()
            .map_or_else(Vec::new, |identity| {
                vec![Request::Log {
                    query: self.log_query(),
                    revision: self.log_revision,
                    identity: identity.clone(),
                    range: None,
                    from: Some(from),
                }]
            })
    }

    fn submit_log_search(&mut self, input: &str, scope: SearchScope) -> Vec<Request> {
        self.search_revision += 1;
        match validate_query(input) {
            Ok(()) => vec![Request::Search {
                workspace: self.workspace.clone(),
                repository: (scope != SearchScope::Workspace)
                    .then(|| self.repository.clone())
                    .flatten(),
                step: (scope == SearchScope::Step)
                    .then(|| self.inspector.logs.identity.clone())
                    .flatten(),
                query: input.to_owned(),
                revision: self.search_revision,
                scope,
            }],
            Err(error) => {
                self.notice = Notice::error(error.to_string());
                Vec::new()
            }
        }
    }

    fn modal_key(&mut self, key: KeyEvent) -> Vec<Request> {
        if key.code == KeyCode::Esc {
            self.modal = Modal::None;
            return Vec::new();
        }
        // Taking the modal keeps edits local while allowing navigation to replace the screen.
        let mut modal = std::mem::take(&mut self.modal);
        let requests = match &mut modal {
            Modal::None | Modal::Help | Modal::QuickOpen => Vec::new(),
            Modal::ChooseWorkspace { workspace, save } => match key.code {
                KeyCode::Tab | KeyCode::Up | KeyCode::Down | KeyCode::Char('j' | 'k') => {
                    *save = !*save;
                    Vec::new()
                }
                KeyCode::Enter => return self.choose_workspace(workspace.clone(), *save),
                _ => Vec::new(),
            },
            Modal::Workspace { input, selected } => self.workspace_key(key, input, selected),
            Modal::Search {
                input,
                scope,
                matches,
                selected,
                editing,
            } => {
                if key.code == KeyCode::Tab {
                    self.search_revision += 1;
                    *scope = match scope {
                        SearchScope::Repository => SearchScope::Workspace,
                        SearchScope::Workspace if self.inspector.logs.identity.is_some() => {
                            SearchScope::Step
                        }
                        SearchScope::Step | SearchScope::Workspace => SearchScope::Repository,
                    };
                    *editing = true;
                    matches.clear();
                    Vec::new()
                } else if key.code == KeyCode::Enter && *editing {
                    self.submit_log_search(input, *scope)
                } else if key.code == KeyCode::Enter
                    && let Some(hit) = matches.get(*selected).cloned()
                {
                    self.inspector.logs.highlight = input.clone();
                    self.log_revision += 1;
                    self.inspector.logs.hits = matches
                        .iter()
                        .filter(|result| super::state::same_log(&result.identity, &hit.identity))
                        .map(|result| result.byte_offset)
                        .collect();
                    self.inspector.logs.hit = self
                        .inspector
                        .logs
                        .hits
                        .iter()
                        .position(|offset| *offset == hit.byte_offset)
                        .unwrap_or(0);
                    return self.open_hit(hit);
                } else if !*editing
                    && matches!(
                        key.code,
                        KeyCode::Up | KeyCode::Down | KeyCode::Char('j' | 'k')
                    )
                {
                    *selected = move_index(
                        *selected,
                        matches!(key.code, KeyCode::Up | KeyCode::Char('k')),
                        1,
                        matches.len(),
                    );
                    Vec::new()
                } else {
                    let previous = input.clone();
                    edit(input, key);
                    if *input != previous {
                        self.search_revision += 1;
                        matches.clear();
                        *selected = 0;
                    }
                    *editing = true;
                    Vec::new()
                }
            }
        };
        if matches!(self.modal, Modal::None) {
            self.modal = modal;
        }
        requests
    }

    fn workspace_key(
        &mut self,
        key: KeyEvent,
        input: &mut String,
        selected: &mut usize,
    ) -> Vec<Request> {
        let choices = self.workspace_choices(input);
        match key.code {
            KeyCode::Up => *selected = move_index(*selected, true, 1, choices.len()),
            KeyCode::Down => *selected = move_index(*selected, false, 1, choices.len()),
            KeyCode::Enter => {
                let workspace = choices
                    .get(*selected)
                    .map_or_else(|| input.clone(), |choice| choice.workspace.slug.clone());
                match workspace.parse::<bb_api::Workspace>() {
                    Ok(_) => {
                        self.modal = Modal::ChooseWorkspace {
                            workspace,
                            save: self.default_workspace.is_none(),
                        }
                    }
                    Err(error) => self.notice = Notice::error(error.to_string()),
                }
            }
            KeyCode::PageDown if self.workspaces.next.is_some() && !self.offline => {
                return vec![Request::Page {
                    resource: Resource::Workspaces,
                    next: self.workspaces.next.clone(),
                }];
            }
            _ => {
                edit(input, key);
                *selected = 0;
            }
        }
        Vec::new()
    }

    fn open_hit(&mut self, hit: LogMatch) -> Vec<Request> {
        self.preview = None;
        self.remember_log();
        let identity = hit.identity;
        if !self
            .filtered_runs()
            .iter()
            .any(|index| self.runs.items[*index].uuid == identity.run_uuid)
        {
            self.filter = super::state::Filter::All;
        }
        if let Some(index) = self
            .filtered_runs()
            .iter()
            .position(|index| self.runs.items[*index].uuid == identity.run_uuid)
        {
            self.runs.selected = index;
        }
        self.epoch += 1;
        let changed_repository = self.repository.as_deref() != Some(&identity.repository);
        if changed_repository {
            self.runs = super::state::Panel::default();
            self.repositories = super::state::Panel::default();
            self.project = ProjectScope::All;
            self.browse.repositories = super::browse::Finder::default();
            self.projects.selected = 1;
        }
        self.repository = Some(identity.repository.clone());
        self.inspector.ancestors.clear();
        self.inspector
            .ancestors
            .insert(identity.root_run_uuid.clone());
        self.focus = Focus::Logs;
        self.inspector.tree = Tree {
            root: identity.root_run_uuid.clone(),
            ..Tree::default()
        };
        self.inspector
            .tree
            .expanded
            .insert(identity.root_run_uuid.clone());
        self.inspector
            .tree
            .expanded
            .insert(identity.run_uuid.clone());
        self.inspector
            .inspection_target
            .clone_from(&identity.run_uuid);
        self.inspector.selection = TreeSelection::Seek {
            run: identity.run_uuid.clone(),
            step: identity.step_uuid.clone(),
        };
        self.inspector.logs.identity = Some(identity.clone());
        self.inspector.logs.window = LogWindow::default();
        self.inspector.logs.scroll = 0;
        self.inspector.logs.follow = false;
        self.inspector.logs.zoom = false;
        self.inspector.logs.horizontal = 0;
        self.inspector.logs.incremental = true;
        self.modal = Modal::None;
        let mut requests = vec![
            Request::Log {
                query: self.log_query(),
                revision: self.log_revision,
                identity: identity.clone(),
                range: None,
                from: Some(hit.byte_offset),
            },
            Request::page(Resource::Run {
                repository: identity.repository.clone(),
                uuid: identity.root_run_uuid.clone(),
            }),
            Request::page(Resource::Children(identity.repository.clone())),
        ];
        if changed_repository {
            requests.push(Request::page(Resource::Runs(identity.repository.clone())));
            requests.extend(self.reload_find(FindTarget::Repositories));
        }
        if identity.run_uuid != identity.root_run_uuid {
            requests.push(Request::page(Resource::Run {
                repository: identity.repository,
                uuid: identity.run_uuid,
            }));
        }
        requests
    }
}

pub(super) fn move_index(index: usize, up: bool, amount: usize, length: usize) -> usize {
    if up {
        index.saturating_sub(amount)
    } else {
        index.saturating_add(amount).min(length.saturating_sub(1))
    }
}

pub(super) fn edit(input: &mut String, key: KeyEvent) {
    match key.code {
        KeyCode::Backspace => {
            input.pop();
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => input.clear(),
        KeyCode::Char(ch)
            if !ch.is_control()
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && input.chars().count() < 1024 =>
        {
            input.push(ch);
        }
        _ => {}
    }
}
