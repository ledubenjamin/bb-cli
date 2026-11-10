//! An event-driven terminal explorer with bounded, cancellable background work.

mod browse;
mod keys;
mod navigation;
mod service;
mod state;
mod tree;
mod ui;
mod viewport;

use anyhow::{Context, Result, bail};
use bb_api::Client;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{
    FutureExt, StreamExt,
    future::{AbortHandle, Abortable, Aborted, LocalBoxFuture},
    stream::FuturesUnordered,
};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, IsTerminal},
    time::Duration,
};
use tokio::sync::mpsc;

use crate::{
    app::App,
    args::{Cli, TuiArgs},
    log_cache::{Cache, now},
};
use service::{Progress, Request, Response};
use state::State;

pub fn validate(cli: &Cli) -> Result<()> {
    if cli.json.is_some() || cli.jq.is_some() || cli.non_interactive || cli.dry_run || cli.verbose {
        bail!(
            "bb tui requires an interactive terminal and does not support --json, --jq, --non-interactive, --dry-run, or --verbose; use bb run list or bb logs search for machine output"
        );
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(
            "bb tui requires a terminal on stdin and stdout; use bb logs search for redirected output"
        );
    }
    Ok(())
}

struct Terminal {
    inner: ratatui::DefaultTerminal,
}
impl Terminal {
    fn open() -> Result<Self> {
        match ratatui::try_init() {
            Ok(inner) => Ok(Self { inner }),
            Err(error) => {
                ratatui::restore();
                Err(error).context("cannot enter terminal interface")
            }
        }
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

pub async fn run(app: &App<'_>, args: &TuiArgs) -> Result<u8> {
    let cache = Cache::for_app(app).await?;
    let (client, expires) = if args.offline {
        (None, None)
    } else {
        let credentials = crate::auth::credential(app.cli, &app.store, &app.config, false)
            .await?
            .context("run bb auth login or set BB_TOKEN; use --offline to browse cached data")?;
        (
            Some(crate::app::client(app.cli, Some(&credentials))?),
            credentials.expires_at,
        )
    };
    let workspace = app.workspace().await.ok();
    let origin = if app.cli.workspace.is_some() {
        "argument/environment"
    } else if app.cli.repo.is_some() {
        "repository"
    } else if app
        .cli
        .context
        .as_ref()
        .or(app.config.active_context.as_ref())
        .is_some()
    {
        "context"
    } else if app.config.preferences.default_workspace.is_some() {
        "default"
    } else {
        "Git"
    };
    let repository = app
        .repo()
        .await
        .ok()
        .filter(|repository| {
            workspace
                .as_ref()
                .is_some_and(|workspace| repository.workspace() == workspace)
        })
        .map(ToString::to_string);
    let default_workspace = app
        .config
        .preferences
        .default_workspace
        .as_ref()
        .map(ToString::to_string);
    let mut state = State::new(workspace, origin.into(), default_workspace, args.offline);
    state.browse.api_origin = crate::favorites::origin(app)?;
    state.browse.favorites = crate::favorites::repositories(
        &app.config.preferences.favorite_repositories,
        &state.browse.api_origin,
    );
    let initial = state.start(repository);
    let mut scheduler = Scheduler::new(app, cache, client, expires);
    scheduler.reconcile(&state);
    scheduler.enqueue(initial, false);
    let mut terminal = Terminal::open()?;
    event_loop(&mut terminal, &mut state, &mut scheduler).await
}

struct Active {
    request: Request,
    abort: AbortHandle,
    cancelled: bool,
}
struct Finished {
    ticket: u64,
    epoch: u64,
    request: Request,
    result: Result<Result<Response>, Aborted>,
}

struct Scheduler<'a> {
    app: &'a App<'a>,
    cache: Cache,
    client: Option<Client>,
    expires: Option<u64>,
    retry_auth: u64,
    epoch: u64,
    sequence: u64,
    active: HashMap<u64, Active>,
    queue: VecDeque<Request>,
    futures: FuturesUnordered<LocalBoxFuture<'a, Finished>>,
    progress_tx: mpsc::Sender<Progress>,
    progress_rx: mpsc::Receiver<Progress>,
}

impl<'a> Scheduler<'a> {
    fn new(app: &'a App<'a>, cache: Cache, client: Option<Client>, expires: Option<u64>) -> Self {
        let (progress_tx, progress_rx) = mpsc::channel(8);
        Self {
            app,
            cache,
            client,
            expires,
            retry_auth: 0,
            epoch: 0,
            sequence: 0,
            active: HashMap::new(),
            queue: VecDeque::new(),
            futures: FuturesUnordered::new(),
            progress_tx,
            progress_rx,
        }
    }

    fn enqueue(&mut self, requests: Vec<Request>, manual: bool) {
        for request in requests {
            if manual && matches!(request, Request::Log { .. } | Request::Search { .. }) {
                let search = matches!(request, Request::Search { .. });
                self.cancel(|pending| {
                    if search {
                        matches!(pending, Request::Search { .. })
                    } else {
                        matches!(pending, Request::Log { .. })
                    }
                });
            }
            let key = request.key();
            if self
                .active
                .values()
                .any(|active| !active.cancelled && active.request.key() == key)
                || self.queue.iter().any(|request| request.key() == key)
            {
                continue;
            }
            if self.queue.len() >= 128 {
                break;
            }
            if matches!(request, Request::Log { .. } | Request::RefreshAuth) {
                self.queue.push_front(request);
            } else {
                self.queue.push_back(request);
            }
        }
    }

    fn cancel(&mut self, obsolete: impl Fn(&Request) -> bool) {
        for active in self
            .active
            .values_mut()
            .filter(|active| obsolete(&active.request))
        {
            active.cancelled = true;
            active.abort.abort();
        }
        self.queue.retain(|request| !obsolete(request));
    }

    fn reconcile(&mut self, state: &State) {
        if self.epoch != state.epoch {
            // Preference writes and still-relevant catalogs outlive navigation.
            self.cancel(|request| {
                !request.persistent()
                    && !matches!(request, Request::Find { .. } if state.find_is_current(request))
            });
            self.epoch = state.epoch;
        }
        self.cancel(|request| match request {
            Request::Find { .. } => !state.find_is_current(request),
            Request::Log { .. } => !state.log_is_current(request),
            Request::Search { .. } => !state.search_is_current(request),
            _ => false,
        });
    }

    fn fill(&mut self) {
        while self.active.len() < 4 {
            let writing = self
                .active
                .values()
                .any(|active| active.request.persistent());
            let Some(index) = self
                .queue
                .iter()
                .position(|request| !writing || !request.persistent())
            else {
                break;
            };
            let Some(request) = self.queue.remove(index) else {
                break;
            };
            self.sequence += 1;
            let ticket = self.sequence;
            let epoch = self.epoch;
            let (abort, registration) = AbortHandle::new_pair();
            self.active.insert(
                ticket,
                Active {
                    request: request.clone(),
                    abort,
                    cancelled: false,
                },
            );
            let cache = self.cache.clone();
            let client = self.client.clone();
            let progress = self.progress_tx.clone();
            let app = self.app;
            self.futures.push(
                async move {
                    let result = Abortable::new(
                        service::execute(
                            app,
                            &cache,
                            client.as_ref(),
                            &request,
                            epoch,
                            ticket,
                            &progress,
                        ),
                        registration,
                    )
                    .await;
                    Finished {
                        ticket,
                        epoch,
                        request,
                        result,
                    }
                }
                .boxed_local(),
            );
        }
    }

    fn complete(&mut self, state: &mut State, finished: Finished) {
        let active = self.active.remove(&finished.ticket);
        let retained = finished.request.persistent()
            || matches!(&finished.request, Request::Find { .. } if state.find_is_current(&finished.request));
        if (finished.epoch != state.epoch && !retained)
            || active.is_none_or(|active| active.cancelled)
        {
            return;
        }
        match finished.result {
            Ok(Ok(Response::Auth { client, expires })) => {
                self.client = Some(client);
                self.expires = expires;
            }
            Ok(Ok(response)) => {
                let requests = state.accept(&finished.request, response);
                self.enqueue(requests, false);
            }
            Ok(Err(error)) => state.fail(&finished.request, &format!("{error:#}")),
            Err(_) => {}
        }
    }

    fn has_preferences(&self) -> bool {
        self.active
            .values()
            .any(|active| active.request.persistent())
            || self.queue.iter().any(Request::persistent)
    }

    async fn flush_preferences(
        &mut self,
        state: &mut State,
        input: &mut EventStream,
    ) -> Result<u8> {
        self.cancel(|request| !request.persistent());
        let mut failure = None;
        loop {
            self.fill();
            if !self.has_preferences() {
                break;
            }
            let finished = tokio::select! {
                finished = self.futures.next() => finished,
                event = input.next() => {
                    match event.transpose().context("cannot read terminal input while closing")? {
                        Some(Event::Key(key)) if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(2),
                        None => return Ok(2),
                        _ => continue,
                    }
                }
            };
            if let Some(mut finished) = finished {
                finished.result = match finished.result {
                    Ok(Err(error)) if finished.request.persistent() => {
                        failure = Some(error);
                        Err(Aborted)
                    }
                    result => result,
                };
                self.complete(state, finished);
            } else {
                break;
            }
        }
        failure
            .map_or(Ok(0), Err)
            .context("cannot save preferences before closing")
    }
}

async fn event_loop(
    terminal: &mut Terminal,
    state: &mut State,
    scheduler: &mut Scheduler<'_>,
) -> Result<u8> {
    let theme = ui::Theme::new(
        std::env::var_os("NO_COLOR").is_none() && std::env::var("TERM").as_deref() != Ok("dumb"),
    );
    let mut input = EventStream::new();
    let mut redraw = tokio::time::interval(Duration::from_millis(50));
    let mut logs = tokio::time::interval(Duration::from_secs(2));
    let mut status = tokio::time::interval(Duration::from_secs(5));
    for timer in [&mut redraw, &mut logs, &mut status] {
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    }
    let mut dirty = true;
    let mut view = viewport::View::default();
    state.schedule_preview();
    loop {
        scheduler.fill();
        tokio::select! {
            event = input.next() => {
                let Some(event) = event else { return scheduler.flush_preferences(state, &mut input).await; };
                match event.context("cannot read terminal input")? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        let (exit, requests) = state.key(key, view.page_size(state.focus));
                        if let Some(code) = exit {
                            if code == 0 {
                                if scheduler.has_preferences() {
                                    state.notice = state::Notice::info("Saving preferences before exit · Ctrl+C Cancel");
                                    terminal.inner.draw(|frame| ui::draw(frame, state, &mut view, 0, theme))?;
                                }
                                return scheduler.flush_preferences(state, &mut input).await;
                            }
                            return Ok(code);
                        }
                        scheduler.reconcile(state);
                        scheduler.enqueue(requests, true);
                    }
                    Event::Resize(_, _) => {}
                    _ => continue,
                }
                dirty = true;
            }
            Some(finished) = scheduler.futures.next(), if !scheduler.futures.is_empty() => {
                scheduler.complete(state, finished); scheduler.reconcile(state); dirty = true;
            }
            Some(progress) = scheduler.progress_rx.recv() => {
                if progress.epoch == state.epoch && let Some(active) = scheduler.active.get(&progress.ticket) && !active.cancelled {
                    let requests = state.accept(&active.request, progress.response);
                    scheduler.reconcile(state);
                    scheduler.enqueue(requests, false);
                    dirty = true;
                }
            }
            _ = logs.tick() => { scheduler.enqueue(state.poll_log(), false); }
            _ = status.tick() => {
                scheduler.enqueue(state.poll_status(), false);
                if scheduler.expires.is_some_and(|expires| expires <= now().saturating_add(60)) && scheduler.retry_auth <= now() {
                    scheduler.enqueue(vec![Request::RefreshAuth], false); scheduler.retry_auth = now().saturating_add(30);
                }
                dirty = true;
            }
            _ = redraw.tick() => {
                let mut requests = state.preview_due(std::time::Instant::now());
                requests.extend(state.find_due(std::time::Instant::now()));
                scheduler.reconcile(state);
                if !requests.is_empty() { scheduler.enqueue(requests, false); dirty = true; }
                if dirty {
                    terminal.inner.draw(|frame|ui::draw(frame, state, &mut view, scheduler.active.len() + scheduler.queue.len(), theme))?;
                    dirty = false;
                }
            }
        }
    }
}

#[cfg(test)]
mod browse_tests;
#[cfg(test)]
mod navigation_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod log_state_tests;
