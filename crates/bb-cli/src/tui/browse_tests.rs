use super::state::Modal;
use super::*;
use crate::{
    discovery::{CatalogItem, CatalogKind},
    log_cache::CachedPage,
};
use bb_api::models::{Project, Repository};
use browse::{FindSource, FindTarget, ProjectEntry};
use crossterm::event::KeyEvent;

fn state(offline: bool) -> Result<State> {
    let mut state = State::new(
        Some("acme".parse()?),
        "default".into(),
        Some("acme".into()),
        offline,
    );
    state.browse.favorites = vec!["acme/service".parse()?, "other/worker".parse()?];
    Ok(state)
}

fn repository(slug: &str) -> CatalogItem {
    CatalogItem::Repository(Box::new(Repository {
        uuid: slug.into(),
        name: slug.into(),
        slug: slug.into(),
        full_name: format!("acme/{slug}"),
        project: Some(Project {
            key: "APP".into(),
            name: "Applications".into(),
            ..Project::default()
        }),
        ..Repository::default()
    }))
}

fn snapshot(values: Vec<CatalogItem>, cached: bool) -> Response {
    Response::Catalog(service::Snapshot {
        page: CachedPage {
            values,
            next: None,
            updated_at: 42,
        },
        cached,
        warning: None,
    })
}

fn key(state: &mut State, code: KeyCode) -> Vec<Request> {
    state.key(KeyEvent::new(code, KeyModifiers::NONE), 24).1
}

#[test]
fn favorites_startup_and_explicit_repository_preserve_navigation_priority() -> Result<()> {
    let mut state = state(true)?;
    state.start(None);
    assert_eq!(state.project, browse::ProjectScope::Favorites);
    assert_eq!(state.focus, state::Focus::Repositories);
    assert_eq!(state.repositories.items[0].full_name, "acme/service");
    let mut explicit = self::state(true)?;
    explicit.start(Some("acme/explicit".into()));
    assert_eq!(explicit.focus, state::Focus::Runs);
    assert_eq!(explicit.repository.as_deref(), Some("acme/explicit"));
    explicit.choose_workspace("other".into(), false);
    assert_eq!(explicit.repositories.items[0].full_name, "other/worker");
    Ok(())
}

#[test]
fn quick_search_discards_old_queries_and_late_cache_results() -> Result<()> {
    let mut state = state(false)?;
    state.open_quick();
    let old = key(&mut state, KeyCode::Char('s')).remove(0);
    assert!(state.find_due(std::time::Instant::now()).is_empty());
    let old_network = state
        .find_due(std::time::Instant::now() + Duration::from_secs(1))
        .remove(0);
    let current = key(&mut state, KeyCode::Char('e')).remove(0);
    state.accept(&old, snapshot(vec![repository("obsolete")], true));
    state.accept(&old_network, snapshot(vec![repository("obsolete")], false));
    assert!(state.browse.quick.results.items.is_empty());
    state.accept(&current, snapshot(vec![repository("service")], true));
    let network = state
        .find_due(std::time::Instant::now() + Duration::from_secs(1))
        .remove(0);
    state.accept(
        &network,
        snapshot(vec![repository("server"), repository("service")], false),
    );
    assert_eq!(state.browse.quick.results.selected, 1);
    state.accept(&current, snapshot(vec![repository("old-cache")], true));
    assert_eq!(state.browse.quick.results.items.len(), 2);
    key(&mut state, KeyCode::Enter);
    assert_eq!(state.repository.as_deref(), Some("acme/service"));
    assert_eq!(state.focus, state::Focus::Runs);
    Ok(())
}

#[test]
fn contextual_filters_keep_project_scope_and_logs_remain_accessible() -> Result<()> {
    let mut state = state(false)?;
    state.open_project(&ProjectEntry::Project(Project {
        key: "APP".into(),
        ..Project::default()
    }));
    key(&mut state, KeyCode::Char('/'));
    let request = key(&mut state, KeyCode::Char('a')).remove(0);
    assert!(
        matches!(&request, Request::Find { query, target: FindTarget::Repositories, source: FindSource::Cache, .. } if query.project.as_deref() == Some("APP") && query.text == "a")
    );
    key(&mut state, KeyCode::Enter);
    assert_eq!(state.browse.repositories.text, "a");
    assert!(state.browse.editing.is_none());
    key(&mut state, KeyCode::Esc);
    assert_eq!(state.browse.repositories.text, "");
    assert_eq!(state.focus, state::Focus::Repositories);
    key(&mut state, KeyCode::Char('L'));
    assert!(matches!(state.modal, Modal::Search { .. }));
    Ok(())
}

#[test]
fn empty_favorites_and_project_quick_open_work_offline() -> Result<()> {
    let mut state = state(true)?;
    state.browse.favorites.clear();
    state.start(None);
    assert_eq!(state.focus, state::Focus::Projects);
    state.open_quick();
    let request = key(&mut state, KeyCode::Tab).remove(0);
    assert_eq!(state.browse.quick.kind, CatalogKind::Projects);
    state.accept(
        &request,
        snapshot(
            vec![CatalogItem::Project(Project {
                key: "APP".into(),
                ..Project::default()
            })],
            true,
        ),
    );
    let requests = key(&mut state, KeyCode::Enter);
    assert_eq!(state.project.project(), Some("APP"));
    assert_eq!(state.focus, state::Focus::Repositories);
    assert!(requests.iter().all(|request| matches!(
        request,
        Request::Find {
            source: FindSource::Cache,
            ..
        }
    )));
    Ok(())
}

#[test]
fn favorites_are_filtered_by_cached_project_metadata() -> Result<()> {
    let mut state = state(true)?;
    let requests = state.open_project(&ProjectEntry::Favorites);
    state.accept(
        &requests[0],
        snapshot(
            vec![repository("service"), repository("not-a-favorite")],
            true,
        ),
    );
    key(&mut state, KeyCode::Char('/'));
    for c in "applications".chars() {
        key(&mut state, KeyCode::Char(c));
    }
    assert_eq!(state.repositories.items.len(), 1);
    assert_eq!(state.repositories.items[0].full_name, "acme/service");
    Ok(())
}

#[test]
fn favorites_and_quick_open_fit_supported_terminal_sizes() -> Result<()> {
    use ratatui::{Terminal, backend::TestBackend};
    let mut state = state(true)?;
    state.start(None);
    for (width, height) in [(80, 24), (120, 40), (200, 60)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        for modal in [false, true] {
            if modal {
                state.open_quick();
            } else {
                state.modal = Modal::None;
            }
            terminal.draw(|frame| {
                ui::draw(
                    frame,
                    &state,
                    &mut viewport::View::default(),
                    0,
                    ui::Theme::new(false),
                );
            })?;
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect();
            assert!(text.contains("service"));
            assert!(text.contains(if modal { "Quick open" } else { "Favorites" }));
            assert!(text.contains("q Quit"));
        }
    }
    Ok(())
}

#[test]
fn network_failure_before_cache_completion_stops_the_loading_indicator() -> Result<()> {
    let mut state = state(false)?;
    state.open_quick();
    let cached = key(&mut state, KeyCode::Char('s')).remove(0);
    state.browse.quick.finder.phase = browse::FindPhase::Failed;
    state.accept(&cached, snapshot(vec![repository("service")], true));
    assert!(!state.browse.quick.finder.loading());
    assert!(state.browse.quick.results.cached);
    assert_eq!(state.browse.quick.results.items.len(), 1);
    Ok(())
}

#[test]
fn quick_open_requests_a_workspace_when_none_is_selected() {
    let mut state = State::new(None, "session".into(), None, false);
    let requests = state.open_quick();
    assert!(matches!(state.modal, Modal::Workspace { .. }));
    assert!(matches!(
        requests.as_slice(),
        [Request::Page {
            resource: service::Resource::Workspaces,
            ..
        }]
    ));
}

#[test]
fn notices_never_inherit_the_previous_tone() -> Result<()> {
    let mut state = state(true)?;
    key(&mut state, KeyCode::Char('L'));
    for code in [KeyCode::Char('a'), KeyCode::Char('b'), KeyCode::Enter] {
        key(&mut state, code);
    }
    assert!(state.notice.is_error(), "{:?}", state.notice);
    key(&mut state, KeyCode::Esc);
    state.focus = state::Focus::Runs;
    key(&mut state, KeyCode::Char('m'));
    assert_eq!(
        state.notice,
        state::Notice::info("Offline: only downloaded pages are available")
    );
    Ok(())
}

#[test]
fn only_the_current_first_page_decides_a_failed_search() -> Result<()> {
    let mut state = state(false)?;
    let outdated = state.reload_find(FindTarget::Projects).remove(1);
    let requests = state.reload_find(FindTarget::Projects);
    state.fail(&outdated, "timed out");
    assert!(state.browse.projects.loading());
    state.accept(
        &requests[0],
        Response::Catalog(service::Snapshot {
            page: CachedPage {
                values: vec![CatalogItem::Project(Project {
                    key: "APP".into(),
                    ..Project::default()
                })],
                next: Some("page-2".into()),
                updated_at: 42,
            },
            cached: true,
            warning: None,
        }),
    );
    let more = state.more_find(FindTarget::Projects).remove(0);
    state.fail(&more, "timed out");
    assert!(
        state.browse.projects.loading(),
        "the first page is still pending"
    );
    state.fail(&requests[1], "timed out");
    assert_eq!(state.browse.projects.phase, browse::FindPhase::Failed);
    assert!(!state.browse.projects.loading());
    assert!(state.notice.is_error());
    Ok(())
}
