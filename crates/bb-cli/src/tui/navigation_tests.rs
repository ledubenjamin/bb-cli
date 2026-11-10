use super::*;
use bb_api::models::{Project, Repository};
use browse::ProjectEntry;
use crossterm::event::KeyEvent;
use ratatui::{
    Terminal as TestTerminal,
    backend::TestBackend,
    style::{Color, Modifier},
};
use state::{Focus, Screen, TreeSelection};

fn browser() -> Result<State> {
    let mut state = State::new(Some("acme".parse()?), "default".into(), None, true);
    state.projects.items.extend((0..33).map(|i| {
        ProjectEntry::Project(Project {
            key: format!("P{i:02}"),
            name: format!("Project {i:02}"),
            ..Project::default()
        })
    }));
    state.browse.repositories.revision = 1;
    state.repositories.items = vec![Repository {
        full_name: "acme/service".into(),
        name: "Service".into(),
        ..Repository::default()
    }];
    state.repository = Some("acme/service".into());
    state.runs.items = [180, 177]
        .into_iter()
        .map(|number| {
            let tree = tests::tree(number)?;
            tree.runs
                .get(&tree.root)
                .cloned()
                .context("fixture run missing")
        })
        .collect::<Result<_>>()?;
    Ok(state)
}

fn key(state: &mut State, code: KeyCode) -> Vec<Request> {
    state.key(KeyEvent::new(code, KeyModifiers::NONE), 12).1
}

fn screen(terminal: &TestTerminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[test]
fn failed_filter_includes_timeouts_but_not_cancellations_or_unknown_results() -> Result<()> {
    let mut state = browser()?;
    state.runs.items = [
        "SUCCESSFUL",
        "EXPIRED",
        "STOPPED",
        "FAILED",
        "ERROR",
        "FUTURE",
    ]
    .into_iter()
    .map(|result| {
        serde_json::from_value(serde_json::json!({
            "uuid": result, "build_number": 1,
            "state": {"name": "COMPLETED", "result": {"name": result}}
        }))
    })
    .collect::<std::result::Result<_, _>>()?;
    state.focus = Focus::Runs;
    key(&mut state, KeyCode::Char('F'));
    let visible: Vec<_> = state
        .filtered_runs()
        .into_iter()
        .map(|index| state.runs.items[index].uuid.as_str())
        .collect();
    assert_eq!(visible, ["EXPIRED", "FAILED", "ERROR"]);
    Ok(())
}

#[test]
fn timed_out_steps_are_selected_counted_and_reachable_with_their_cause_visible() -> Result<()> {
    let mut state = browser()?;
    let run = state.runs.items[0].clone();
    state.inspect(run.clone());
    let steps = [
        ("Build", "SUCCESSFUL"),
        ("Timeout", "EXPIRED"),
        ("Cancelled", "STOPPED"),
        ("Failed", "FAILED"),
        ("Unknown", "FUTURE"),
    ]
    .into_iter()
    .map(|(name, result)| {
        serde_json::from_value(serde_json::json!({
            "uuid": name, "name": name,
            "state": {"name": "COMPLETED", "result": {"name": result}}
        }))
    })
    .collect::<std::result::Result<_, _>>()?;
    state.inspector.tree.steps.insert(run.uuid, steps);
    state.inspector.tree.children_loaded = true;
    state.update_tree();
    assert_eq!(
        state
            .inspector
            .logs
            .identity
            .as_ref()
            .map(|log| log.step_uuid.as_str()),
        Some("Timeout")
    );
    for (code, expected) in [
        ('e', "Failed"),
        ('e', "Timeout"),
        ('E', "Failed"),
        ('E', "Timeout"),
    ] {
        key(&mut state, KeyCode::Char(code));
        assert_eq!(
            state
                .inspector
                .logs
                .identity
                .as_ref()
                .map(|log| log.step_uuid.as_str()),
            Some(expected)
        );
    }
    // Keep the timeout unselected so its own status style is visible.
    state.inspector.tree_selected = 0;
    for color in [false, true] {
        let mut terminal = TestTerminal::new(TestBackend::new(200, 40))?;
        let mut view = viewport::View::default();
        terminal.draw(|frame| ui::draw(frame, &state, &mut view, 0, ui::Theme::new(color)))?;
        let text = screen(&terminal);
        assert!(text.contains("Steps · 2 failed"));
        assert!(text.contains("! Timeout · attempt 1 EXPIRED"));
        assert!(text.contains("· Cancelled · attempt 1 STOPPED"));
        assert!(text.contains("· Unknown · attempt 1 FUTURE"));
        if color {
            let marker = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .find(|cell| cell.symbol() == "!")
                .context("failure marker missing")?;
            assert_eq!(marker.fg, Color::Red);
        }
    }
    Ok(())
}

#[test]
fn all_thirty_five_projects_stay_visible_at_every_selection() -> Result<()> {
    let mut state = browser()?;
    let mut terminal = TestTerminal::new(TestBackend::new(200, 60))?;
    let mut view = viewport::View::default();
    for selected in 0..35 {
        state.projects.selected = selected;
        terminal.draw(|frame| ui::draw(frame, &state, &mut view, 0, ui::Theme::new(true)))?;
        let text = screen(&terminal);
        for expected in ["Favorites", "All repositories", "Project 00", "Project 32"] {
            assert!(
                text.contains(expected),
                "missing {expected} at selection {selected}"
            );
        }
        assert!(!text.contains("No matching projects"));
        assert!(!text.contains("No matching repositories"));
    }
    Ok(())
}

#[test]
fn viewport_scrolls_at_edges_and_resets_when_everything_fits() {
    let mut viewport = viewport::Viewport::default();
    for selected in 0..8 {
        assert_eq!(viewport.prepare(selected, 35, 8, "projects"), 0);
    }
    assert_eq!(viewport.prepare(8, 35, 8, "projects"), 1);
    assert_eq!(viewport.prepare(4, 35, 8, "projects"), 1);
    assert_eq!(viewport.prepare(34, 35, 8, "projects"), 27);
    assert_eq!(viewport.prepare(34, 35, 40, "projects"), 0);
    assert_eq!(viewport.prepare(2, 3, 8, "filtered"), 0);
    assert_eq!(viewport.prepare(0, 0, 0, "empty"), 0);
}

#[test]
fn arrows_are_spatial_and_inspection_does_not_replace_browser_context() -> Result<()> {
    let mut state = browser()?;
    state.projects.selected = 1;
    key(&mut state, KeyCode::Left);
    assert_eq!(state.focus, Focus::Projects);
    key(&mut state, KeyCode::Right);
    assert_eq!(state.focus, Focus::Repositories);
    key(&mut state, KeyCode::Right);
    assert_eq!(state.focus, Focus::Runs);
    key(&mut state, KeyCode::Enter);
    assert_eq!(state.screen(), Screen::Inspect);
    assert_eq!(state.focus, Focus::Tree);
    key(&mut state, KeyCode::Left);
    key(&mut state, KeyCode::Left);
    assert_eq!(state.focus, Focus::Builds);
    key(&mut state, KeyCode::Right);
    key(&mut state, KeyCode::Right);
    key(&mut state, KeyCode::Right);
    assert_eq!(state.focus, Focus::Logs);
    state.key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT), 12);
    assert_eq!(state.inspector.logs.horizontal, 8);
    key(&mut state, KeyCode::Left);
    assert_eq!(state.focus, Focus::Tree);
    assert_eq!(state.inspector.logs.horizontal, 8);
    key(&mut state, KeyCode::Char('?'));
    key(&mut state, KeyCode::Esc);
    assert_eq!(state.screen(), Screen::Inspect);
    key(&mut state, KeyCode::Esc);
    assert_eq!(state.screen(), Screen::Explore);
    assert_eq!(state.focus, Focus::Runs);
    assert_eq!(state.projects.selected, 1);
    assert_eq!(state.repositories.items.len(), 1);
    assert_eq!(state.runs.items.len(), 2);
    Ok(())
}

#[test]
fn rapid_project_previews_debounce_and_reject_an_obsolete_catalog() -> Result<()> {
    let mut state = browser()?;
    state.projects.selected = 2;
    state.schedule_preview();
    assert!(state.preview_due(std::time::Instant::now()).is_empty());
    key(&mut state, KeyCode::Down);
    let requests = state.preview_due(std::time::Instant::now() + Duration::from_secs(1));
    assert_eq!(state.project.project(), Some("P01"));
    assert_eq!(state.focus, Focus::Projects);
    let obsolete = requests
        .into_iter()
        .find(|request| matches!(request, Request::Find { .. }))
        .context("missing catalog request")?;
    key(&mut state, KeyCode::Down);
    state.preview_due(std::time::Instant::now() + Duration::from_secs(1));
    state.accept(
        &obsolete,
        Response::Catalog(service::Snapshot {
            page: crate::log_cache::CachedPage {
                values: vec![crate::discovery::CatalogItem::Repository(Box::new(
                    Repository {
                        full_name: "acme/obsolete".into(),
                        ..Repository::default()
                    },
                ))],
                next: None,
                updated_at: 1,
            },
            cached: true,
            warning: None,
        }),
    );
    assert_eq!(state.project.project(), Some("P02"));
    assert!(state.repositories.items.is_empty());
    key(&mut state, KeyCode::Down);
    key(&mut state, KeyCode::Right);
    assert_eq!(state.project.project(), Some("P03"));
    assert_eq!(state.focus, Focus::Repositories);
    Ok(())
}

#[test]
fn build_previews_keep_focus_and_return_to_the_last_inspected_build() -> Result<()> {
    let mut state = browser()?;
    state.focus = Focus::Runs;
    key(&mut state, KeyCode::Enter);
    key(&mut state, KeyCode::Left);
    key(&mut state, KeyCode::Down);
    assert!(state.pending_from(Focus::Builds));
    state.preview_due(std::time::Instant::now() + Duration::from_secs(1));
    assert_eq!(state.focus, Focus::Builds);
    assert_eq!(state.inspector.inspection_target, state.runs.items[1].uuid);
    key(&mut state, KeyCode::Esc);
    assert_eq!(state.runs.selected, 1);
    assert_eq!(state.focus, Focus::Runs);
    Ok(())
}

#[test]
fn groups_clear_unrelated_logs_but_pending_search_targets_keep_their_logs() -> Result<()> {
    let mut state = browser()?;
    state.inspector.tree = tests::tree(177)?;
    state.focus = Focus::Tree;
    state.inspector.tree_selected = state
        .inspector
        .tree
        .rows
        .iter()
        .position(|row| row.failed && !row.expandable)
        .context("missing failure")?;
    state.select_log();
    let identity = state
        .inspector
        .logs
        .identity
        .clone()
        .context("missing log")?;
    state.inspector.tree_selected = 0;
    state.inspector.selection = TreeSelection::Seek {
        run: identity.run_uuid.clone(),
        step: identity.step_uuid.clone(),
    };
    state.select_log();
    assert!(state.inspector.logs.identity.is_some());
    state.inspector.selection = TreeSelection::Automatic;
    state.select_log();
    assert!(state.inspector.logs.identity.is_none());
    Ok(())
}

#[test]
fn focus_uses_only_one_filled_row_and_overlay_keeps_breadcrumb_visible() -> Result<()> {
    let mut state = browser()?;
    let mut view = viewport::View::default();
    let mut terminal = TestTerminal::new(TestBackend::new(200, 60))?;
    for focus in [Focus::Projects, Focus::Repositories, Focus::Runs] {
        state.focus = focus;
        terminal.draw(|frame| ui::draw(frame, &state, &mut view, 0, ui::Theme::new(true)))?;
        let rows = terminal
            .backend()
            .buffer()
            .content()
            .chunks(200)
            .filter(|row| row.iter().any(|cell| cell.bg == Color::Cyan))
            .count();
        assert_eq!(rows, 1, "only the focused list has a filled selection");
    }
    state.focus = Focus::Runs;
    key(&mut state, KeyCode::Enter);
    state.inspector.tree = tests::tree(180)?;
    for (width, height) in [(80, 24), (120, 40), (200, 60)] {
        let mut terminal = TestTerminal::new(TestBackend::new(width, height))?;
        for focus in [Focus::Builds, Focus::Tree, Focus::Logs] {
            state.focus = focus;
            terminal.draw(|frame| ui::draw(frame, &state, &mut view, 0, ui::Theme::new(false)))?;
            let text = screen(&terminal);
            assert!(text.contains("Explore ›"));
            assert!(text.contains("Pipeline inspector"));
            assert!(
                text.contains("1 Builds") && text.contains("2 Steps") && text.contains("3 Logs")
            );
            assert!(text.contains("q Quit"));
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .any(|cell| cell.modifier.contains(Modifier::REVERSED))
            );
        }
    }
    Ok(())
}

#[test]
fn leaving_follow_scrolls_one_line_without_jumping_or_waiting_a_page() -> Result<()> {
    let mut state = browser()?;
    state.focus = Focus::Logs;
    state.inspector.logs.follow = true;
    state.inspector.logs.scroll = 99;
    state.inspector.logs.window.lines = (0..100)
        .map(|i| crate::log_cache::LogLine {
            highlights: Vec::new(),
            start: i,
            end: i + 1,
            text: format!("Line {i}"),
        })
        .collect();
    key(&mut state, KeyCode::Up);
    assert!(!state.inspector.logs.follow);
    assert_eq!(state.inspector.logs.scroll, 87);
    key(&mut state, KeyCode::Up);
    assert_eq!(state.inspector.logs.scroll, 86);
    key(&mut state, KeyCode::PageUp);
    assert_eq!(state.inspector.logs.scroll, 74);
    Ok(())
}

/// Optional render artifacts for manual QA; normal tests do not write files.
fn capture(
    terminal: &TestTerminal<TestBackend>,
    name: &str,
    width: u16,
    height: u16,
) -> Result<()> {
    let Some(directory) = std::env::var_os("BB_TEST_RENDER_DIR") else {
        return Ok(());
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory)?;
    let cells: Vec<_> = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| {
            serde_json::json!([
                cell.symbol(),
                format!("{:?}", cell.fg),
                format!("{:?}", cell.bg),
                cell.modifier.bits()
            ])
        })
        .collect();
    std::fs::write(
        directory.join(format!("{name}-{width}x{height}.json")),
        serde_json::to_vec(&serde_json::json!({"width": width, "height": height, "cells": cells}))?,
    )?;
    Ok(())
}

#[test]
fn inspector_renders_readable_step_logs_in_color_and_monochrome() -> Result<()> {
    let mut state = browser()?;
    let mut view = viewport::View::default();
    state.projects.selected = 32;
    state.runs.selected = 1;
    state.focus = Focus::Projects;
    let mut terminal = TestTerminal::new(TestBackend::new(200, 60))?;
    terminal.draw(|frame| ui::draw(frame, &state, &mut view, 0, ui::Theme::new(true)))?;
    capture(&terminal, "browse", 200, 60)?;
    state.focus = Focus::Runs;
    key(&mut state, KeyCode::Enter);
    state.inspector.tree = tests::tree(177)?;
    state.inspector.tree_selected = state
        .inspector
        .tree
        .rows
        .iter()
        .position(|row| row.failed && !row.expandable)
        .context("missing failure")?;
    state.select_log();
    state.inspector.logs.window.lines = (0..18)
        .map(|i| crate::log_cache::LogLine {
            highlights: Vec::new(),
            start: i * 80,
            end: (i + 1) * 80,
            text: if i == 16 {
                "ERROR: compilation failed in child pipeline".into()
            } else {
                format!("Building module {i:02} ...")
            },
        })
        .collect();
    state.inspector.logs.window.updated_at = crate::log_cache::now();
    state.inspector.logs.highlight = "ERROR".into();
    for (width, height) in [(80, 24), (120, 40), (200, 60)] {
        let mut terminal = TestTerminal::new(TestBackend::new(width, height))?;
        for color in [false, true] {
            state.focus = Focus::Logs;
            terminal.draw(|frame| ui::draw(frame, &state, &mut view, 0, ui::Theme::new(color)))?;
            assert!(screen(&terminal).contains("compilation failed"));
            if color {
                capture(&terminal, "inspector", width, height)?;
            }
        }
    }
    Ok(())
}

#[test]
fn tree_interaction_stops_automatic_selection_but_finishes_a_pending_seek() {
    let mut selection = TreeSelection::Automatic;
    selection.stop_automatic();
    assert_eq!(selection, TreeSelection::Manual);
    let seek = TreeSelection::Seek {
        run: "run".into(),
        step: "step".into(),
    };
    let mut selection = seek.clone();
    selection.stop_automatic();
    assert_eq!(selection, seek);
}
