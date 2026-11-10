use super::*;
use crate::log_cache::{CachedPage, LogMatch, LogWindow};
use bb_api::models::{Run, Step};
use crossterm::event::KeyEvent;
use service::Resource;
use state::{Focus, Modal, Panel, SearchScope, TreeSelection};

fn following() -> Result<(State, Request)> {
    let run: Run = serde_json::from_value(
        serde_json::json!({"uuid":"run","build_number":1,"state":{"name":"IN_PROGRESS"}}),
    )?;
    let step: Step = serde_json::from_value(
        serde_json::json!({"uuid":"step","name":"Compile","state":{"name":"IN_PROGRESS"}}),
    )?;
    let mut state = State::new(Some("acme".parse()?), "session".into(), None, false);
    state.repository = Some("acme/repo".into());
    state.focus = Focus::Logs;
    state.inspector.selection = TreeSelection::Manual;
    state.inspector.tree = tree::Tree::new(run);
    state.inspector.tree.steps.insert("run".into(), vec![step]);
    state.inspector.tree.rebuild();
    state.inspector.tree_selected = 1;
    let request = state
        .select_log()
        .pop()
        .context("initial log request missing")?;
    Ok((state, request))
}

fn terminal(state: &mut State) -> Result<Vec<Request>> {
    state
        .inspector
        .tree
        .steps
        .get_mut("run")
        .context("steps missing")?[0]
        .state
        .name = "COMPLETED".into();
    Ok(state.update_tree())
}

fn log_response(finished: bool, total: u64) -> Response {
    Response::Log {
        window: LogWindow {
            total: Some(total),
            ..LogWindow::default()
        },
        from: None,
        incremental: true,
        finished,
    }
}

fn key(state: &mut State, code: KeyCode) -> Vec<Request> {
    state.key(KeyEvent::new(code, KeyModifiers::NONE), 12).1
}

#[test]
fn final_tail_is_not_deduplicated_with_a_request_started_while_running() -> Result<()> {
    let (mut state, active) = following()?;
    let final_request = terminal(&mut state)?
        .pop()
        .context("final log request missing")?;
    assert_ne!(active.key(), final_request.key());
    assert!(!state.log_is_current(&active));
    state.accept(&active, log_response(true, 10));
    assert_eq!(state.inspector.logs.window.total, None);
    assert!(state.inspector.logs.final_tail_pending());
    // Preview and partial progress cannot satisfy a completed download.
    state.accept(&final_request, log_response(false, 20));
    assert!(state.inspector.logs.final_tail_pending());
    assert_eq!(state.poll_log().len(), 1);
    state.accept(&final_request, log_response(true, 30));
    assert_eq!(state.inspector.logs.window.total, Some(30));
    assert!(!state.inspector.logs.final_tail_pending());
    assert_eq!(state.poll_log().len(), 0);
    assert_eq!(state.update_tree().len(), 0);
    Ok(())
}

#[test]
fn final_tail_respects_pause_offline_and_retries_after_failure() -> Result<()> {
    for pause in [false, true] {
        let (mut state, _) = following()?;
        state.inspector.logs.follow = !pause;
        state.offline = !pause;
        assert_eq!(terminal(&mut state)?.len(), 0);
        assert!(state.inspector.logs.final_tail_pending());
        state.offline = false;
        state.inspector.logs.follow = true;
        state.inspector.logs.incremental = false;
        let retry = state.poll_log().pop().context("final retry missing")?;
        // Errors produce no accepted response; the next poll must still request a tail.
        assert_eq!(state.poll_log().len(), 1);
        state.accept(&retry, log_response(true, 1));
        assert_eq!(state.poll_log().len(), 0);
    }
    Ok(())
}

#[test]
fn steps_keep_polling_when_the_run_finishes_before_its_steps() -> Result<()> {
    let (mut state, _) = following()?;
    state
        .inspector
        .tree
        .runs
        .get_mut("run")
        .context("run missing")?
        .state
        .name = "COMPLETED".into();
    assert!(state.poll_status().iter().any(|request| matches!(request,
        Request::Page { resource: Resource::Steps { run, .. }, .. } if run == "run")));
    terminal(&mut state)?;
    assert!(!state.poll_status().iter().any(|request| matches!(
        request,
        Request::Page {
            resource: Resource::Steps { .. },
            ..
        }
    )));
    Ok(())
}

#[test]
fn late_search_cannot_update_modal_or_hits_after_scope_or_text_changes() -> Result<()> {
    let (mut state, _) = following()?;
    key(&mut state, KeyCode::Char('/'));
    for ch in "needle".chars() {
        key(&mut state, KeyCode::Char(ch));
    }
    let old = key(&mut state, KeyCode::Enter)
        .pop()
        .context("search missing")?;
    let hit = LogMatch {
        identity: state
            .inspector
            .logs
            .identity
            .clone()
            .context("log missing")?,
        byte_offset: 42,
        text: "needle".into(),
        log_complete: true,
        updated_at: 1,
    };
    assert!(state.search_is_current(&old));
    for _ in 0..3 {
        key(&mut state, KeyCode::Tab);
        state.accept(&old, Response::Search(vec![hit.clone()]));
        assert_eq!(state.inspector.logs.hits.len(), 0);
        assert_eq!(state.inspector.logs.highlight, "");
        assert!(
            matches!(&state.modal, Modal::Search { matches, editing: true, .. } if matches.is_empty())
        );
    }
    assert!(matches!(
        state.modal,
        Modal::Search {
            scope: SearchScope::Step,
            ..
        }
    ));
    let old = key(&mut state, KeyCode::Enter)
        .pop()
        .context("search missing")?;
    key(&mut state, KeyCode::Backspace);
    key(&mut state, KeyCode::Char('e'));
    state.accept(&old, Response::Search(vec![hit.clone()]));
    assert_eq!(state.inspector.logs.hits.len(), 0);
    let current = key(&mut state, KeyCode::Enter)
        .pop()
        .context("search missing")?;
    let highlights = state.accept(&current, Response::Search(vec![hit]));
    assert_eq!(state.inspector.logs.hits, [42]);
    assert_eq!(state.inspector.logs.highlight, "needle");
    assert!(matches!(
        highlights.as_slice(),
        [Request::Log { query: Some(_), .. }]
    ));
    Ok(())
}

#[test]
fn panel_merge_preserves_order_selection_and_last_duplicate() {
    let mut panel = Panel {
        items: vec![("a", 1), ("b", 2)],
        selected: 1,
        ..Panel::default()
    };
    let snapshot = |values| service::Snapshot {
        page: CachedPage {
            values,
            next: None,
            updated_at: 1,
        },
        cached: false,
        warning: None,
    };
    panel.apply(snapshot(vec![("b", 3), ("c", 4), ("b", 5)]), true, |item| {
        item.0
    });
    assert_eq!(panel.items, [("a", 1), ("b", 5), ("c", 4)]);
    assert_eq!(panel.selected, 1);
    panel.apply(snapshot(vec![("c", 6), ("b", 7)]), false, |item| item.0);
    assert_eq!(panel.items, [("c", 6), ("b", 7)]);
    assert_eq!(panel.selected, 1);
}
