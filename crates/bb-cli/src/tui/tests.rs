use super::*;
use bb_api::models::{Run, Step};
use ratatui::{Terminal, backend::TestBackend};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixtures {
    runs: Vec<Run>,
    steps: HashMap<String, Vec<Step>>,
    earlier: Vec<Earlier>,
}
#[derive(Deserialize)]
struct Earlier {
    parent: String,
    step: Step,
}

fn fixture() -> Result<Fixtures> {
    Ok(serde_json::from_str(include_str!(
        "../../tests/fixtures/pipelines.json"
    ))?)
}

pub(super) fn tree(number: u64) -> Result<tree::Tree> {
    let fixtures = fixture()?;
    let root = fixtures
        .runs
        .iter()
        .find(|run| run.build_number == number)
        .cloned()
        .context("fixture root missing")?;
    let mut tree = tree::Tree::new(root);
    for run in fixtures.runs {
        tree.expanded.insert(run.uuid.clone());
        tree.runs.insert(run.uuid.clone(), run);
    }
    tree.steps = fixtures.steps;
    for old in fixtures.earlier {
        tree.earlier
            .insert((old.parent, old.step.uuid.clone()), old.step);
    }
    for (parent, steps) in &tree.steps {
        for step in steps {
            tree.expanded.insert(format!("{parent}:{}", step.uuid));
        }
    }
    tree.children_loaded = true;
    tree.expand_failures();
    tree.rebuild();
    Ok(tree)
}

#[test]
fn sibling_children_and_eight_child_steps_stay_inside_parent_tree() -> Result<()> {
    let tree = tree(180)?;
    assert_eq!(
        tree.rows
            .iter()
            .filter(|row| matches!(row.node, tree::Node::Run(_)))
            .count(),
        3
    );
    assert_eq!(
        tree.rows
            .iter()
            .filter(|row| matches!(row.node, tree::Node::Step { .. }))
            .count(),
        18
    );
    assert!(!tree.rows.iter().any(|row| row.label == "Earlier triggers"));
    Ok(())
}

#[test]
fn rerun_uses_original_trigger_uuid_even_when_current_step_has_same_name() -> Result<()> {
    let tree = tree(177)?;
    let old = tree
        .rows
        .iter()
        .position(|row| row.label == "Earlier triggers")
        .context("missing earlier section")?;
    let child = tree
        .rows
        .iter()
        .position(|row| row.label.starts_with("#178 "))
        .context("missing child")?;
    assert!(child > old);
    let failed = tree
        .rows
        .iter()
        .find(|row| row.label.starts_with("Build arm64 image"))
        .context("missing failed step")?;
    let identity = tree
        .identity("acme/service", &failed.node)
        .context("missing identity")?;
    assert_eq!(identity.run_number, 178);
    assert_eq!(identity.run_attempt, 2);
    assert_eq!(identity.step_attempt, 2);
    assert_eq!(identity.root_run_uuid, tree.root);
    assert_eq!(identity.execution_path.len(), 4);
    let current = tree
        .rows
        .iter()
        .find(|row| row.label == "Engine release · attempt 2")
        .context("missing rerun step")?;
    assert!(!current.expandable);
    Ok(())
}

#[test]
fn terminal_layouts_keep_navigation_and_logs_visible() -> Result<()> {
    let mut state = State::new(
        Some("acme".parse()?),
        "default".into(),
        Some("acme".into()),
        true,
    );
    state.inspector.tree = tree(177)?;
    state.repository = Some("acme/service".into());
    state.focus = state::Focus::Tree;
    state.inspector.tree_selected = state
        .inspector
        .tree
        .rows
        .iter()
        .position(|row| row.label.starts_with("Build arm64 image"))
        .context("missing failed step")?;
    state.select_log();
    for (width, height) in [(80, 24), (120, 40), (200, 60)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        state.focus = state::Focus::Tree;
        let mut view = viewport::View::default();
        terminal.draw(|frame| ui::draw(frame, &state, &mut view, 0, ui::Theme::new(false)))?;
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(screen.contains("Steps"));
        assert!(screen.contains("Build arm64"));
        assert!(screen.contains("q Quit"));
        state.focus = state::Focus::Logs;
        terminal.draw(|frame| ui::draw(frame, &state, &mut view, 0, ui::Theme::new(true)))?;
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(screen.contains("Logs"));
        assert!(screen.contains("No cached logs"));
    }
    Ok(())
}

#[test]
fn workspace_choice_preserves_explicit_session_and_default_actions() -> Result<()> {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut state = State::new(
        Some("acme".parse()?),
        "default".into(),
        Some("acme".into()),
        true,
    );
    state.modal = state::Modal::ChooseWorkspace {
        workspace: "other".into(),
        save: false,
    };
    let (_, requests) = state.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 24);
    assert_eq!(state.workspace, "other");
    assert_eq!(state.default_workspace.as_deref(), Some("acme"));
    assert!(
        !requests
            .iter()
            .any(|request| matches!(request, Request::SaveWorkspace(_)))
    );
    state.modal = state::Modal::ChooseWorkspace {
        workspace: "persistent".into(),
        save: true,
    };
    let (_, requests) = state.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 24);
    assert!(requests.iter().any(
        |request| matches!(request,Request::SaveWorkspace(workspace) if workspace == "persistent")
    ));
    assert_eq!(
        state
            .key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), 24)
            .0,
        Some(2)
    );
    Ok(())
}

#[test]
fn opening_a_child_directly_resolves_its_parent_and_can_collapse_again() -> Result<()> {
    use crate::log_cache::CachedPage;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use service::{Resource, Snapshot};
    let fixtures = fixture()?;
    let child = fixtures
        .runs
        .iter()
        .find(|run| run.build_number == 181)
        .cloned()
        .context("child missing")?;
    let root = fixtures
        .runs
        .iter()
        .find(|run| run.build_number == 180)
        .cloned()
        .context("root missing")?;
    let mut state = State::new(Some("acme".parse()?), "default".into(), None, true);
    state.repository = Some("acme/service".into());
    state.inspect(child.clone());
    assert_eq!(state.inspector.tree.root, root.uuid);
    state.accept(
        &Request::page(Resource::Run {
            repository: "acme/service".into(),
            uuid: root.uuid.clone(),
        }),
        Response::Runs(Snapshot {
            page: CachedPage {
                values: vec![root.clone()],
                next: None,
                updated_at: 1,
            },
            cached: true,
            warning: None,
        }),
    );
    for run in [&root, &child] {
        state.accept(
            &Request::page(Resource::Steps {
                repository: "acme/service".into(),
                run: run.uuid.clone(),
            }),
            Response::Steps(Snapshot {
                page: CachedPage {
                    values: fixtures
                        .steps
                        .get(&run.uuid)
                        .context("steps missing")?
                        .clone(),
                    next: None,
                    updated_at: 1,
                },
                cached: true,
                warning: None,
            }),
        );
    }
    state.accept(
        &Request::page(Resource::Children("acme/service".into())),
        Response::Runs(Snapshot {
            page: CachedPage {
                values: fixtures.runs,
                next: None,
                updated_at: 1,
            },
            cached: true,
            warning: None,
        }),
    );
    let identity = state
        .inspector
        .logs
        .identity
        .as_ref()
        .context("child log was not selected")?;
    assert_eq!(identity.run_number, 181);
    assert_eq!(identity.root_run_uuid, root.uuid);
    assert_eq!(
        identity.parent_step_uuid.as_deref(),
        Some("{00000000-0000-4000-8000-000000001803}")
    );
    state.inspector.tree_selected = state
        .inspector
        .tree
        .rows
        .iter()
        .position(|row| matches!(&row.node, tree::Node::Run(uuid) if uuid == &child.uuid))
        .context("child row missing")?;
    state.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), 40);
    assert!(!state.inspector.tree.expanded.contains(&child.uuid));
    Ok(())
}
