//! Persistent, virtualized list viewports. Selection never recenters a short list.

use ratatui::widgets::ListState;

use super::state::Focus;

#[derive(Debug, Default)]
pub struct Viewport {
    state: ListState,
    pub height: usize,
    context: String,
}

impl Viewport {
    pub fn scroll_to(&mut self, first: usize, length: usize, height: u16, context: &str) -> usize {
        self.prepare(first, length, height, context);
        let first = first.min(length.saturating_sub(usize::from(height)));
        *self.state.offset_mut() = first;
        first
    }

    pub fn prepare(&mut self, selected: usize, length: usize, height: u16, context: &str) -> usize {
        if self.context != context {
            self.context.clear();
            self.context.push_str(context);
            self.state = ListState::default();
        }
        self.height = usize::from(height);
        let selected = selected.min(length.saturating_sub(1));
        self.state.select((length > 0).then_some(selected));
        let height = self.height.max(1);
        let mut offset = self.state.offset().min(length.saturating_sub(height));
        if selected < offset {
            offset = selected;
        } else if selected >= offset.saturating_add(height) {
            offset = selected.saturating_sub(height - 1);
        }
        *self.state.offset_mut() = offset;
        offset
    }
}

#[derive(Debug, Default)]
pub struct View {
    pub projects: Viewport,
    pub repositories: Viewport,
    pub runs: Viewport,
    pub builds: Viewport,
    pub steps: Viewport,
    pub logs: Viewport,
    pub quick: Viewport,
    pub workspaces: Viewport,
    pub search: Viewport,
}

impl View {
    pub fn page_size(&self, focus: Focus) -> usize {
        match focus {
            Focus::Projects => self.projects.height,
            Focus::Repositories => self.repositories.height,
            Focus::Runs => self.runs.height,
            Focus::Builds => self.builds.height,
            Focus::Tree => self.steps.height,
            Focus::Logs => self.logs.height,
        }
        .max(1)
    }
}
