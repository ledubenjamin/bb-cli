//! Delayed previews and spatial keyboard navigation share explicit transitions.

use std::time::{Duration, Instant};

use super::{
    service::Request,
    state::{Focus, Modal, Screen, State, log_key},
};

#[derive(Debug)]
pub struct Preview {
    pub focus: Focus,
    key: String,
    due: Instant,
}

impl State {
    pub fn focused(&self, focus: Focus) -> bool {
        self.focus == focus && matches!(self.modal, Modal::None)
    }

    pub fn pending_from(&self, focus: Focus) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|preview| preview.focus == focus)
    }

    pub fn schedule_preview(&mut self) {
        if !matches!(self.modal, Modal::None) || self.browse.editing.is_some() {
            return;
        }
        let selection = match self.focus {
            Focus::Projects => self
                .projects
                .items
                .get(self.projects.selected)
                .map(|project| {
                    (
                        project.key().to_owned(),
                        project.key() == self.project.key()
                            && self.browse.repositories.revision > 0,
                    )
                }),
            Focus::Repositories => {
                self.repositories
                    .items
                    .get(self.repositories.selected)
                    .map(|repo| {
                        (
                            repo.full_name.clone(),
                            self.repository.as_ref() == Some(&repo.full_name),
                        )
                    })
            }
            Focus::Builds => self
                .filtered_runs()
                .get(self.runs.selected)
                .and_then(|index| self.runs.items.get(*index))
                .map(|run| {
                    (
                        run.uuid.clone(),
                        self.inspector.inspection_target == run.uuid,
                    )
                }),
            _ => None,
        };
        let Some((key, false)) = selection else {
            self.preview = None;
            return;
        };
        if self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.focus == self.focus && preview.key == key)
        {
            return;
        }
        self.preview = Some(Preview {
            focus: self.focus,
            key,
            due: Instant::now() + Duration::from_millis(250),
        });
    }

    pub fn preview_due(&mut self, now: Instant) -> Vec<Request> {
        if !matches!(self.modal, Modal::None) || self.browse.editing.is_some() {
            return Vec::new();
        }
        if self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.due <= now)
        {
            self.flush_preview()
        } else {
            Vec::new()
        }
    }

    pub fn flush_preview(&mut self) -> Vec<Request> {
        let Some(preview) = self.preview.take() else {
            return Vec::new();
        };
        let focus = self.focus;
        let requests = match preview.focus {
            Focus::Projects => self
                .projects
                .items
                .get(self.projects.selected)
                .cloned()
                .filter(|project| project.key() == preview.key)
                .map_or_else(Vec::new, |project| self.open_project(&project)),
            Focus::Repositories => self
                .repositories
                .items
                .get(self.repositories.selected)
                .map(|repo| repo.full_name.clone())
                .filter(|repo| *repo == preview.key)
                .map_or_else(Vec::new, |repo| self.open_repository(repo)),
            Focus::Builds => self
                .filtered_runs()
                .get(self.runs.selected)
                .and_then(|index| self.runs.items.get(*index))
                .cloned()
                .filter(|run| run.uuid == preview.key)
                .map_or_else(Vec::new, |run| self.inspect(run)),
            _ => Vec::new(),
        };
        self.focus = focus;
        requests
    }

    pub fn focus_pane(&mut self, reverse: bool, wrap: bool) -> Vec<Request> {
        let mut requests = self.flush_preview();
        let order = if self.screen() == Screen::Explore {
            [Focus::Projects, Focus::Repositories, Focus::Runs]
        } else {
            [Focus::Builds, Focus::Tree, Focus::Logs]
        };
        let index = order
            .iter()
            .position(|focus| *focus == self.focus)
            .unwrap_or(0);
        let next = if wrap {
            (index + if reverse { 2 } else { 1 }) % 3
        } else if reverse {
            index.saturating_sub(1)
        } else {
            (index + 1).min(2)
        };
        self.focus = order[next];
        self.inspector.logs.zoom = false;
        self.schedule_preview();
        // Entering a repository immediately starts its first pipeline preview.
        if self.focus == Focus::Repositories {
            requests.extend(self.flush_preview());
        }
        requests
    }

    pub fn remember_log(&mut self) {
        if let Some(identity) = &self.inspector.logs.identity
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
                .insert(log_key(identity), line.start);
        }
    }

    pub fn focus_number(&mut self, index: usize) -> Vec<Request> {
        let order = if self.screen() == Screen::Explore {
            [Focus::Projects, Focus::Repositories, Focus::Runs]
        } else {
            [Focus::Builds, Focus::Tree, Focus::Logs]
        };
        let Some(target) = order.get(index) else {
            return Vec::new();
        };
        let mut requests = Vec::new();
        for _ in 0..2 {
            if self.focus == *target {
                break;
            }
            let current = order
                .iter()
                .position(|focus| *focus == self.focus)
                .unwrap_or(0);
            requests.extend(self.focus_pane(current > index, false));
        }
        requests
    }
}
