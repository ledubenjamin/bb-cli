use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{
    browse::{FindTarget, ProjectEntry, ProjectScope},
    state::{Focus, Modal, Screen, State},
    tree,
    viewport::{View, Viewport},
};
use crate::discovery::{CatalogItem, CatalogKind};
use crate::log_cache::{clean_inline, now};
use crate::pipeline_status::Status;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub accent: Style,
    pub error: Style,
    pub muted: Style,
    pub success: Style,
    pub warning: Style,
    pub selection: Style,
    color: bool,
}
impl Theme {
    pub fn new(color: bool) -> Self {
        Self {
            accent: if color {
                Style::new().fg(Color::Cyan).bold()
            } else {
                Style::new().bold()
            },
            error: if color {
                Style::new().fg(Color::Red).bold()
            } else {
                Style::new().bold()
            },
            muted: Style::new().add_modifier(Modifier::DIM),
            success: if color {
                Style::new().fg(Color::Green)
            } else {
                Style::new()
            },
            warning: if color {
                Style::new().fg(Color::Yellow)
            } else {
                Style::new()
            },
            selection: if color {
                Style::new().fg(Color::Black).bg(Color::Cyan).bold()
            } else {
                Style::new().bold().reversed()
            },
            color,
        }
    }

    fn inspector(self) -> Self {
        Self {
            accent: if self.color {
                Style::new().fg(Color::LightMagenta).bold()
            } else {
                self.accent
            },
            selection: if self.color {
                Style::new().fg(Color::Black).bg(Color::LightMagenta).bold()
            } else {
                self.selection
            },
            ..self
        }
    }

    fn status(self, status: &str) -> Style {
        let status = Status::from_name(status);
        if status.is_failure() {
            self.error
        } else if status == Status::Successful {
            self.success
        } else if status.is_running() || status == Status::Paused {
            self.warning
        } else {
            self.muted
        }
    }
}

pub fn draw(frame: &mut Frame<'_>, state: &State, view: &mut View, active: usize, theme: Theme) {
    let area = frame.area();
    if area.width < 40 || area.height < 12 {
        frame.render_widget(
            Paragraph::new("Terminal too small\nResize to at least 40 × 12\nq Quit"),
            area,
        );
        return;
    }
    let [header, content, notice, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(2),
    ])
    .areas(area);
    let workspace = if state.workspace.is_empty() {
        "Choose a workspace"
    } else {
        &state.workspace
    };
    let mode = if state.offline { "OFFLINE" } else { "ONLINE" };
    let title = Line::from(vec![
        Span::styled(" bb ", theme.accent),
        Span::raw(format!("{} [{}]", clean_inline(workspace), state.origin)),
        Span::styled(
            if active == 0 {
                format!("   {mode}")
            } else {
                format!("   {mode}  {active} loading")
            },
            theme.muted,
        ),
    ]);
    let breadcrumb = format!(
        " Explore › {} › {}",
        clean_inline(match &state.project {
            ProjectScope::All => "All projects",
            ProjectScope::Favorites => "Favorites",
            ProjectScope::Project(key) => key,
        }),
        clean_inline(state.repository.as_deref().unwrap_or("Choose a repository"))
    );
    frame.render_widget(Paragraph::new(vec![title, Line::from(breadcrumb)]), header);
    explore(frame, content, state, view, theme);
    frame.render_widget(
        Paragraph::new(clean_inline(state.notice.text())).style(if state.notice.is_error() {
            theme.error
        } else {
            theme.accent
        }),
        notice,
    );
    let hint = if state.screen() == Screen::Explore {
        if state.focus == Focus::Runs {
            " ↑↓ Select  ←→ Pane  Enter Inspect  F Filter  ? Help\n Ctrl+P Quick open  * Favorite  L Search logs  w Workspace  q Quit"
        } else {
            " ↑↓ Select  ←→ Pane  / Find  * Favorite  ? Help\n Ctrl+P Quick open  w Workspace  m More  q Quit"
        }
    } else {
        " Inspector open · Esc returns to Pipelines\n Ctrl+P Quick open  w Workspace  ? Help  q Quit"
    };
    frame.render_widget(Paragraph::new(hint).style(theme.muted), footer);
    if state.screen() == Screen::Inspect {
        dim(frame);
        inspect(frame, content, state, view, theme.inspector());
    }
    modal(
        frame,
        area,
        state,
        view,
        if state.screen() == Screen::Inspect {
            theme.inspector()
        } else {
            theme
        },
    );
}

fn explore(frame: &mut Frame<'_>, area: Rect, state: &State, view: &mut View, theme: Theme) {
    if area.width >= 110 {
        let columns = Layout::horizontal([
            Constraint::Percentage(23),
            Constraint::Percentage(28),
            Constraint::Percentage(49),
        ])
        .split(area);
        projects(frame, columns[0], state, &mut view.projects, theme);
        repositories(frame, columns[1], state, &mut view.repositories, theme);
        runs(frame, columns[2], state, &mut view.runs, Focus::Runs, theme);
    } else {
        let [tabs, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        let labels = [
            (Focus::Projects, "1 Projects"),
            (Focus::Repositories, "2 Repositories"),
            (Focus::Runs, "3 Pipelines"),
        ];
        frame.render_widget(
            Paragraph::new(Line::from(
                labels
                    .into_iter()
                    .map(|(focus, label)| {
                        Span::styled(
                            format!(" {label} "),
                            if state.focused(focus) {
                                theme.accent.reversed()
                            } else {
                                theme.muted
                            },
                        )
                    })
                    .collect::<Vec<_>>(),
            )),
            tabs,
        );
        match state.focus {
            Focus::Projects => projects(frame, body, state, &mut view.projects, theme),
            Focus::Repositories => repositories(frame, body, state, &mut view.repositories, theme),
            _ => runs(frame, body, state, &mut view.runs, Focus::Runs, theme),
        }
    }
}

fn projects(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    viewport: &mut Viewport,
    theme: Theme,
) {
    let area = filter_bar(frame, area, state, FindTarget::Projects, theme);
    let title = format!(
        "Projects · {}{}",
        state
            .projects
            .items
            .iter()
            .filter(|entry| matches!(entry, ProjectEntry::Project(_)))
            .count(),
        source(state.projects.cached, state.projects.updated)
    );
    list(
        frame,
        area,
        &title,
        state.projects.selected,
        state.projects.items.len(),
        state.focused(Focus::Projects),
        theme,
        |index| {
            let item = &state.projects.items[index];
            match item {
                ProjectEntry::Favorites => Line::styled(
                    format!("* Favorites ({})", state.favorite_repositories().len()),
                    theme.accent,
                ),
                ProjectEntry::All => Line::raw("All repositories"),
                ProjectEntry::Project(project) => Line::raw(format!(
                    "{}  {}",
                    clean_inline(&project.key),
                    clean_inline(&project.name)
                )),
            }
        },
        if state.browse.projects.loading() {
            "Searching…"
        } else if state.projects.cached {
            "Cached results only · r Refresh"
        } else if state.projects.next.is_some() {
            "m Load more projects"
        } else if state.projects.items.is_empty() {
            "No matching projects · / Find · r Refresh"
        } else {
            "↑↓ Preview · → Repositories"
        },
        viewport,
        &format!("{}:{}", state.workspace, state.browse.projects.text),
    );
}

fn repositories(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    viewport: &mut Viewport,
    theme: Theme,
) {
    if state.pending_from(Focus::Projects) {
        pending(
            frame,
            area,
            "Repositories",
            "Waiting for project selection…",
            state.focused(Focus::Repositories),
            theme,
        );
        return;
    }
    let area = filter_bar(frame, area, state, FindTarget::Repositories, theme);
    let title = format!(
        "{} · {}{}",
        if state.project == ProjectScope::Favorites {
            "Favorites"
        } else {
            "Repositories"
        },
        state.repositories.items.len(),
        source(state.repositories.cached, state.repositories.updated)
    );
    list(
        frame,
        area,
        &title,
        state.repositories.selected,
        state.repositories.items.len(),
        state.focused(Focus::Repositories),
        theme,
        |index| {
            let item = &state.repositories.items[index];
            repository_line(item, state, theme)
        },
        if state.project == ProjectScope::Favorites {
            if state.repositories.items.is_empty() {
                "No matching favorites · Ctrl+P Find repositories"
            } else {
                "* Toggle favorite · / Find"
            }
        } else if state.browse.repositories.loading() {
            "Searching…"
        } else if state.repositories.cached {
            "Cached results only · r Refresh"
        } else if state.repositories.next.is_some() {
            "m Load more repositories"
        } else if state.repositories.items.is_empty() {
            "No matching repositories · Ctrl+P Quick open"
        } else {
            "↑↓ Preview · → Pipelines"
        },
        viewport,
        &format!(
            "{}:{:?}:{}",
            state.workspace, state.project, state.browse.repositories.text
        ),
    );
}

fn repository_line(
    item: &bb_api::models::Repository,
    state: &State,
    theme: Theme,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            if state.is_favorite(&item.full_name) {
                "* "
            } else {
                "  "
            },
            theme.accent,
        ),
        Span::raw(clean_inline(if item.name.is_empty() {
            &item.full_name
        } else {
            &item.name
        })),
        Span::styled(
            item.project.as_ref().map_or_else(String::new, |project| {
                format!("  [{}]", clean_inline(&project.key))
            }),
            theme.muted,
        ),
    ])
}

fn filter_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    target: FindTarget,
    theme: Theme,
) -> Rect {
    let finder = state.browse.finder(target);
    let editing = state.browse.editing == Some(target);
    if finder.text.is_empty() && !editing {
        return area;
    }
    let [prompt, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(format!(
            "{} · Enter Apply · Esc Clear\n> {}",
            if editing { "Find" } else { "Filter" },
            clean_inline(&finder.text)
        ))
        .style(theme.accent),
        prompt,
    );
    if editing {
        cursor(frame, prompt, &finder.text);
    }
    body
}

fn runs(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    viewport: &mut Viewport,
    focus: Focus,
    theme: Theme,
) {
    let label = if focus == Focus::Builds {
        "Builds"
    } else {
        "Pipelines"
    };
    if state.pending_from(Focus::Projects) || state.pending_from(Focus::Repositories) {
        pending(
            frame,
            area,
            label,
            "Waiting for repository selection…",
            state.focused(focus),
            theme,
        );
        return;
    }
    let indices = state.filtered_runs();
    let title = format!(
        "{label} · {} · {}/{} loaded{}",
        state.filter.label(),
        indices.len(),
        state.runs.items.len(),
        source(state.runs.cached, state.runs.updated)
    );
    list(
        frame,
        area,
        &title,
        state.runs.selected,
        indices.len(),
        state.focused(focus),
        theme,
        |index| {
            let run = &state.runs.items[indices[index]];
            let status = tree::status(&run.state);
            let mut spans = vec![Span::styled(
                if area.width >= 45 {
                    format!("#{:<5} {status:<11}", run.build_number)
                } else {
                    format!("#{:<4} {status}", run.build_number)
                },
                theme.status(&status),
            )];
            if area.width >= 45 {
                spans.push(Span::raw(format!(
                    " {:<16} {}{}",
                    clean_inline(&run.target.ref_name.chars().take(16).collect::<String>()),
                    clean_inline(&run.created_on.chars().take(16).collect::<String>()),
                    if run.trigger.name == "PARENT_STEP" {
                        "  child"
                    } else {
                        ""
                    }
                )));
            }
            Line::from(spans)
        },
        if state.runs.next.is_some() {
            "m Load more · filters apply to loaded runs"
        } else if indices.is_empty() && state.repository.is_none() {
            "Select a repository to see its pipelines"
        } else if indices.is_empty() && state.runs_loading {
            "Loading pipelines…"
        } else if indices.is_empty() {
            "No matching runs · F Filter / open a repository"
        } else {
            "F Filter · r Refresh"
        },
        viewport,
        &format!("{:?}:{:?}", state.repository, state.filter),
    );
}

fn inspect(frame: &mut Frame<'_>, area: Rect, state: &State, view: &mut View, theme: Theme) {
    let margin = (area.width / 20).max(1);
    let area = Rect::new(
        area.x + margin,
        area.y + 1,
        area.width.saturating_sub(margin * 2),
        area.height.saturating_sub(2),
    );
    frame.render_widget(Clear, area);
    let run = state
        .inspector
        .tree
        .runs
        .get(&state.inspector.inspection_target);
    let title = format!(
        "Pipeline inspector · {} · #{} · Esc Close",
        clean_inline(state.repository.as_deref().unwrap_or("")),
        run.map_or_else(|| "…".into(), |run| run.build_number.to_string())
    );
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(format!(" {title} "))
        .border_style(theme.accent);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [summary, tabs, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .areas(inner);
    let details = run.map_or_else(
        || "Loading execution…".into(),
        |run| {
            format!(
                "{}  ·  {}  ·  {}",
                tree::status(&run.state),
                clean_inline(&run.target.ref_name),
                clean_inline(&run.created_on)
            )
        },
    );
    frame.render_widget(
        Paragraph::new(details)
            .style(run.map_or(theme.muted, |run| theme.status(&tree::status(&run.state)))),
        summary,
    );
    frame.render_widget(
        Paragraph::new(Line::from(
            [
                (Focus::Builds, "1 Builds"),
                (Focus::Tree, "2 Steps"),
                (Focus::Logs, "3 Logs"),
            ]
            .into_iter()
            .map(|(focus, label)| {
                Span::styled(
                    format!(" {label} "),
                    if state.focused(focus) {
                        theme.selection
                    } else {
                        theme.muted
                    },
                )
            })
            .collect::<Vec<_>>(),
        )),
        tabs,
    );
    let hint = match state.focus {
        Focus::Builds => {
            "↑↓ Preview build  → Steps  F Filter  m More\nEsc Close  Tab Pane  ? Help  q Quit"
        }
        Focus::Tree => {
            "↑↓ Preview logs  ←→ Pane  Enter Expand  e/E Failure\nEsc Close  / Search logs  z Zoom  ? Help  q Quit"
        }
        _ => {
            "↑↓ Scroll  ← Steps  Shift+←→ Pan  f Follow\nEsc Back  / Search  z Zoom  ? Help  q Quit"
        }
    };
    frame.render_widget(Paragraph::new(hint).style(theme.muted), footer);
    if state.inspector.logs.zoom {
        logs(frame, body, state, &mut view.logs, theme);
    } else if frame.area().width >= 120 {
        let [builds, steps, logs_area] = Layout::horizontal([
            Constraint::Percentage(20),
            Constraint::Percentage(30),
            Constraint::Percentage(50),
        ])
        .areas(body);
        runs(frame, builds, state, &mut view.builds, Focus::Builds, theme);
        execution(frame, steps, state, &mut view.steps, theme);
        logs(frame, logs_area, state, &mut view.logs, theme);
    } else {
        match state.focus {
            Focus::Builds => runs(frame, body, state, &mut view.builds, Focus::Builds, theme),
            Focus::Tree => execution(frame, body, state, &mut view.steps, theme),
            _ => logs(frame, body, state, &mut view.logs, theme),
        }
    }
}

fn execution(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    viewport: &mut Viewport,
    theme: Theme,
) {
    if state.pending_from(Focus::Builds) {
        pending(
            frame,
            area,
            "Steps",
            "Waiting for build selection…",
            state.focused(Focus::Tree),
            theme,
        );
        return;
    }
    let failed = state
        .inspector
        .tree
        .rows
        .iter()
        .filter(|row| row.failed && !row.expandable)
        .count();
    let title = format!(
        "Steps · {failed} failed{}",
        if state.inspector.tree.children_loaded {
            ""
        } else if state.offline {
            " · child index not cached"
        } else {
            " · finding children…"
        }
    );
    list(
        frame,
        area,
        &title,
        state.inspector.tree_selected,
        state.inspector.tree.rows.len(),
        state.focused(Focus::Tree),
        theme,
        |index| {
            let row = &state.inspector.tree.rows[index];
            let marker = if row.expandable {
                if row.open { "▾" } else { "▸" }
            } else {
                if row.failed {
                    "!"
                } else if Status::from_name(&row.status) == Status::Successful {
                    "✓"
                } else {
                    "·"
                }
            };
            let text = format!(
                "{}{} {} {}",
                "│ ".repeat(row.depth.min(12)),
                marker,
                row.label,
                row.status
            );
            Line::styled(text, theme.status(&row.status))
        },
        "Enter/Space Expand · ← Builds · → Logs",
        viewport,
        &format!(
            "{:?}:{}",
            state.repository, state.inspector.inspection_target
        ),
    );
}

fn logs(frame: &mut Frame<'_>, area: Rect, state: &State, viewport: &mut Viewport, theme: Theme) {
    if state.pending_from(Focus::Builds) {
        pending(
            frame,
            area,
            "Logs",
            "Waiting for build selection…",
            state.focused(Focus::Logs),
            theme,
        );
        return;
    }
    let Some(identity) = &state.inspector.logs.identity else {
        let context = state
            .inspector
            .tree
            .rows
            .get(state.inspector.tree_selected)
            .map_or_else(String::new, |row| clean_inline(&row.label));
        frame.render_widget(
            Paragraph::new(format!(
                "{context}\nSelect a step to read its logs.\nEnter/Space expands child pipelines."
            ))
            .block(panel("Logs", state.focused(Focus::Logs), theme)),
            area,
        );
        return;
    };
    let title = format!(
        "Logs · #{} · {} · attempt {}",
        identity.run_number, identity.step_name, identity.step_attempt
    );
    let block = panel(&title, state.focused(Focus::Logs), theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [metadata, lines, coverage] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let window = &state.inspector.logs.window;
    let size: u64 = window.ranges.iter().map(|(start, end)| end - start).sum();
    let completeness = if window.complete {
        "complete"
    } else {
        "partial"
    };
    let following = if !state.inspector.logs.incremental {
        "Range unavailable · r Reload"
    } else if state.inspector.logs.follow {
        "FOLLOW"
    } else {
        "paused"
    };
    let meta = format!(
        "{}\n{completeness} · {} KiB cached · Updated {} · {following}",
        identity.execution_path.join(" › "),
        size / 1024,
        age(window.updated_at)
    );
    frame.render_widget(Paragraph::new(meta).style(theme.muted), metadata);
    if window.lines.is_empty() {
        frame.render_widget(
            Paragraph::new(if state.offline {
                "No cached logs for this step."
            } else {
                "Loading logs… (steps that have not started have no log)"
            }),
            lines,
        );
    } else {
        log_lines(frame, lines, &state.inspector.logs, viewport, theme);
    }
    let first = window.lines.first().map_or(0, |line| line.start);
    let last = window.lines.last().map_or(0, |line| line.end);
    let total = window
        .total
        .map_or_else(|| "?".into(), |size| size.to_string());
    frame.render_widget(
        Paragraph::new(format!(
            "bytes {first}–{last} / {total} · PgUp older · Home/End"
        ))
        .style(theme.muted),
        coverage,
    );
}

fn log_lines(
    frame: &mut Frame<'_>,
    area: Rect,
    logs: &super::state::Logs,
    viewport: &mut Viewport,
    theme: Theme,
) {
    let Some(identity) = &logs.identity else {
        return;
    };
    let first = if logs.follow {
        logs.window
            .lines
            .len()
            .saturating_sub(usize::from(area.height))
    } else {
        logs.scroll
    };
    let start = viewport.scroll_to(
        first,
        logs.window.lines.len(),
        area.height,
        &format!(
            "{}:{}:{}:{}",
            identity.repository, identity.run_uuid, identity.step_uuid, identity.step_attempt
        ),
    );
    let content: Vec<_> = logs
        .window
        .lines
        .iter()
        .skip(start)
        .take(usize::from(area.height))
        .map(|line| {
            let prefix = format!("{:>8} │ ", line.start);
            let width = usize::from(area.width).saturating_sub(prefix.width());
            let mut spans = vec![Span::styled(prefix, theme.muted)];
            spans.extend(highlight(line, usize::from(logs.horizontal), width, theme));
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(content), area);
}

/// Clips in terminal cells and styles whole graphemes without changing source spans.
fn highlight(
    line: &crate::log_cache::LogLine,
    horizontal: usize,
    width: usize,
    theme: Theme,
) -> Vec<Span<'_>> {
    let mut spans = Vec::new();
    let mut column = 0;
    let mut used = 0;
    let mut segment: Option<(usize, usize, bool)> = None;
    for (offset, grapheme) in line.text.grapheme_indices(true) {
        let cells = grapheme.width();
        column += cells;
        if column.saturating_sub(cells) < horizontal {
            continue;
        }
        if used + cells > width {
            break;
        }
        used += cells;
        let end = offset + grapheme.len();
        let index = line.highlights.partition_point(|span| span.end <= offset);
        let active = line
            .highlights
            .get(index)
            .is_some_and(|span| span.start < end);
        if let Some((start, stop, previous)) = segment
            && previous != active
        {
            spans.push(Span::styled(
                &line.text[start..stop],
                if previous {
                    theme.accent.reversed()
                } else {
                    Style::default()
                },
            ));
            segment = None;
        }
        segment = Some((segment.map_or(offset, |(start, _, _)| start), end, active));
    }
    if let Some((start, end, active)) = segment {
        spans.push(Span::styled(
            &line.text[start..end],
            if active {
                theme.accent.reversed()
            } else {
                Style::default()
            },
        ));
    }
    spans
}

fn modal(frame: &mut Frame<'_>, area: Rect, state: &State, view: &mut View, theme: Theme) {
    if matches!(state.modal, Modal::None) {
        return;
    }
    dim(frame);
    let width = area.width.saturating_sub(4).min(110);
    let height = area.height.saturating_sub(4).min(30);
    let area = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    match &state.modal {
        Modal::None => {}
        Modal::QuickOpen => quick_open(frame, area, state, &mut view.quick, theme),
        Modal::Help => {
            let text = "Navigation\n  ↑↓ / jk Select and preview   ←→ / hl Previous/next pane\n  Enter Open   Tab / Shift+Tab Cycle panes\n  Ctrl+P Quick open (Tab Repositories / Projects)\n  * Favorite   / Find in Projects / Repositories\n  F Filter loaded builds   m More   w Workspace   r Refresh\nPipeline inspector: Builds → Steps → Logs\n  ↑↓ Preview builds/steps, scroll logs   ←→ Change pane\n  Enter/Space Expand children   e/E Failed step\n  Shift+←→ Pan logs   f Follow   z Zoom\n  PgUp/PgDn Page   Home/End Start/Tail\nLogs\n  L Search logs anywhere   / Search in inspector\n  Tab Scope   Enter Search/Open   n/N Next/previous hit\n  Searches downloaded portions only (3–1024 characters).\n  Esc Close top window / clear filter   q Quit   Ctrl+C Interrupt\nUpdated/Cached times show data freshness, not build duration.\nOffline: cached results only. Logs: 7 days, 500 MiB.";
            frame.render_widget(
                Paragraph::new(text).block(panel("Keyboard help · Esc Close", true, theme)),
                area,
            );
        }
        Modal::ChooseWorkspace { workspace, save } => {
            let text = vec![
                Line::raw(format!("Workspace: {}", clean_inline(workspace))),
                Line::raw(""),
                Line::styled(
                    "  Use for this session",
                    if *save {
                        Style::new()
                    } else {
                        theme.accent.reversed()
                    },
                ),
                Line::styled(
                    "  Set as default",
                    if *save {
                        theme.accent.reversed()
                    } else {
                        Style::new()
                    },
                ),
                Line::raw(""),
                Line::raw("↑↓ / Tab Choose · Enter Apply · Esc Cancel"),
            ];
            frame.render_widget(
                Paragraph::new(text).block(panel("Workspace preference", true, theme)),
                area,
            );
        }
        Modal::Workspace { input, selected } => {
            let block = panel("Workspace · Enter Choose · Esc Close", true, theme);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let [prompt, body, help] = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .areas(inner);
            frame.render_widget(
                Paragraph::new(format!(
                    "Filter or type a workspace slug:\n> {}",
                    clean_inline(input)
                )),
                prompt,
            );
            cursor(frame, prompt, input);
            let choices = state.workspace_choices(input);
            bare_list(
                frame,
                body,
                *selected,
                choices.len(),
                |index| {
                    Line::raw(format!(
                        "{}  {}",
                        clean_inline(&choices[index].workspace.slug),
                        clean_inline(&choices[index].workspace.name)
                    ))
                },
                theme,
                true,
                &mut view.workspaces,
                input,
            );
            frame.render_widget(Paragraph::new("↑↓ Select · PgDn More workspaces\nIf listing is unavailable, type the exact workspace slug.").style(theme.muted), help);
        }
        Modal::Search { .. } => search_modal(frame, area, state, &mut view.search, theme),
    }
}

fn quick_open(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    viewport: &mut Viewport,
    theme: Theme,
) {
    let quick = &state.browse.quick;
    let block = panel(
        &format!(
            "Quick open · {} · Tab Switch · Esc Close",
            quick.kind.label()
        ),
        true,
        theme,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [prompt, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .areas(inner);
    frame.render_widget(
        Paragraph::new(format!(
            "Workspace: {}\n> {}",
            clean_inline(&state.workspace),
            clean_inline(&quick.finder.text)
        )),
        prompt,
    );
    cursor(frame, prompt, &quick.finder.text);
    if quick.results.items.is_empty() {
        let empty = if quick.finder.loading() {
            "Searching…"
        } else if quick.kind == CatalogKind::Repositories && quick.finder.text.is_empty() {
            "Your favorites appear here. Type to find a repository."
        } else {
            "No matching results"
        };
        frame.render_widget(Paragraph::new(empty).style(theme.muted), body);
    } else {
        bare_list(
            frame,
            body,
            quick.results.selected,
            quick.results.items.len(),
            |index| match &quick.results.items[index] {
                CatalogItem::Repository(repository) => repository_line(repository, state, theme),
                CatalogItem::Project(project) => Line::raw(format!(
                    "{}  {}",
                    clean_inline(&project.key),
                    clean_inline(&project.name)
                )),
            },
            theme,
            true,
            viewport,
            &format!("{}:{:?}:{}", state.workspace, quick.kind, quick.finder.text),
        );
    }
    let status = if quick.finder.loading() {
        "Searching Bitbucket…"
    } else if quick.results.cached || state.offline {
        "Cached results only"
    } else if quick.results.next.is_some() {
        "PgDn More results"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{} results · {status}\n↑↓ Select · Enter Open · * Favorite · PgDn More\n{}",
            quick.results.items.len(),
            clean_inline(state.notice.text())
        ))
        .style(theme.muted),
        footer,
    );
}

fn search_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &State,
    viewport: &mut Viewport,
    theme: Theme,
) {
    let Modal::Search {
        input,
        scope,
        matches,
        selected,
        editing,
    } = &state.modal
    else {
        return;
    };
    let block = panel(
        &format!("Search logs · {} · Tab Scope · Esc Close", scope.label()),
        true,
        theme,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [prompt, body, help] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .areas(inner);
    frame.render_widget(
        Paragraph::new(format!(
            "{}\n> {}",
            if *editing {
                "Enter to search downloaded log portions"
            } else {
                "Enter to open · type to search again"
            },
            clean_inline(input)
        )),
        prompt,
    );
    if *editing {
        cursor(frame, prompt, input);
    }
    bare_list(
        frame,
        body,
        *selected,
        matches.len(),
        |index| {
            let hit = &matches[index];
            Line::raw(format!(
                "{} #{} · {} · {}",
                hit.identity.repository, hit.identity.run_number, hit.identity.step_name, hit.text
            ))
        },
        theme,
        true,
        viewport,
        &format!("{}:{:?}:{input}", state.workspace, scope),
    );
    frame.render_widget(Paragraph::new(format!("{} matches (maximum 1000) · ↑↓ Select\nOnly downloaded portions are indexed; absent text may be uncached.", matches.len())).style(theme.muted), help);
}

fn cursor(frame: &mut Frame<'_>, area: Rect, input: &str) {
    let offset = u16::try_from(input.width())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(area.width.saturating_sub(1));
    frame.set_cursor_position((area.x + offset, area.y + 1));
}

#[expect(
    clippy::too_many_arguments,
    reason = "The list renderer takes immutable presentation inputs and renders only the visible rows"
)]
fn list(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    selected: usize,
    length: usize,
    focused: bool,
    theme: Theme,
    row: impl Fn(usize) -> Line<'static>,
    hint: &str,
    viewport: &mut Viewport,
    context: &str,
) {
    let block = panel(title, focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    viewport.prepare(selected, length, body.height, context);
    if length == 0 {
        frame.render_widget(Paragraph::new(hint).style(theme.muted), body);
    } else {
        bare_list(
            frame, body, selected, length, row, theme, focused, viewport, context,
        );
    }
    frame.render_widget(
        Paragraph::new(if length == 0 {
            String::new()
        } else {
            format!("{}/{} · {hint}", selected + 1, length)
        })
        .style(theme.muted),
        footer,
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "Virtualized lists share viewport, focus and row presentation inputs"
)]
fn bare_list(
    frame: &mut Frame<'_>,
    area: Rect,
    selected: usize,
    length: usize,
    row: impl Fn(usize) -> Line<'static>,
    theme: Theme,
    focused: bool,
    viewport: &mut Viewport,
    context: &str,
) {
    let first = viewport.prepare(selected, length, area.height, context);
    let items: Vec<_> = (first..length)
        .take(usize::from(area.height))
        .map(|index| {
            let mut line = row(index);
            let style = if index == selected && focused {
                theme.selection
            } else if index == selected {
                theme.muted
            } else {
                Style::new()
            };
            if index == selected && focused {
                for span in &mut line.spans {
                    span.style = theme.selection;
                }
            }
            line.spans.insert(
                0,
                Span::raw(if index == selected {
                    if focused { "› " } else { "· " }
                } else {
                    "  "
                }),
            );
            ListItem::new(line).style(style)
        })
        .collect();
    frame.render_widget(List::new(items), area);
}

fn panel(title: &str, focused: bool, theme: Theme) -> Block<'static> {
    Block::new()
        .borders(Borders::ALL)
        .title(format!(" {}{title} ", if focused { "› " } else { "" }))
        .border_type(if focused {
            BorderType::Double
        } else {
            BorderType::Plain
        })
        .border_style(if focused { theme.accent } else { theme.muted })
}

fn source(cached: bool, updated: u64) -> String {
    if updated == 0 {
        String::new()
    } else {
        format!(
            " · {} {}",
            if cached { "Cached" } else { "Updated" },
            age(updated)
        )
    }
}

fn dim(frame: &mut Frame<'_>) {
    let area = frame.area();
    frame.buffer_mut().set_style(
        area,
        Style::new()
            .add_modifier(Modifier::DIM)
            .remove_modifier(Modifier::BOLD | Modifier::REVERSED),
    );
}

fn pending(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    message: &str,
    focused: bool,
    theme: Theme,
) {
    frame.render_widget(
        Paragraph::new(message)
            .style(theme.muted)
            .block(panel(title, focused, theme)),
        area,
    );
}

fn age(updated: u64) -> String {
    if updated == 0 {
        return "not downloaded".into();
    }
    let seconds = now().saturating_sub(updated);
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

#[cfg(test)]
mod unicode_tests {
    use super::*;
    use crate::log_cache::LogLine;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn clipping_keeps_wide_combining_and_emoji_graphemes_intact() {
        let text = "界e\u{301}👩‍💻 NEEDLE needle";
        let at = text.find("NEEDLE").unwrap_or(0);
        let line = LogLine {
            start: 0,
            end: text.len() as u64,
            text: text.into(),
            highlights: vec![at..at + 6, at + 7..at + 13],
        };
        let spans = highlight(&line, 2, 3, Theme::new(false));
        assert_eq!(
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "e\u{301}👩‍💻"
        );
        let spans = highlight(&line, 8, 11, Theme::new(false));
        assert_eq!(
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "EDLE needle"
        );
        assert_eq!(
            spans
                .iter()
                .filter(|span| span.style.add_modifier.contains(Modifier::REVERSED))
                .count(),
            2
        );
    }

    #[test]
    fn search_cursor_uses_terminal_width() -> anyhow::Result<()> {
        let mut terminal = Terminal::new(TestBackend::new(40, 5))?;
        terminal.draw(|frame| cursor(frame, Rect::new(0, 0, 40, 3), "界e\u{301}👩‍💻"))?;
        assert_eq!(
            terminal.get_cursor_position()?,
            ratatui::layout::Position::new(7, 1)
        );
        Ok(())
    }
}
