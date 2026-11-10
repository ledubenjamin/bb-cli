# Design

`bb-api` owns HTTP, typed resource decoding, identifier validation, canonical errors, pagination, and explicit request bodies. It does not read environment variables, manage credentials, initialize logging, or print to the terminal. The concrete async `Client` shares its immutable configuration, secret, and reusable reqwest pool through a private `Arc`; cloning it does not duplicate the token. Rustls supplies TLS. Mutable client-wide defaults are avoided.

`bb-cli` owns command parsing, repository/context resolution, operating system credentials, Git, confirmations, response normalization, output schemas, and exit codes. clap is the source of the command tree. schemars derives resource contracts from the representations used by output. Subcommand handlers share a small `App` with lazy authentication and repository resolution; an offline schema command never initializes it.

The terminal UI remains inside `bb-cli`. Ratatui rendering and keyboard state are separate from cancellable I/O jobs. Four concurrent jobs and bounded queues limit background work. SQLite runs on a dedicated thread; schema serialization, queries, writes, retention, and search indexing do not run in the render loop. A dropped request interrupts only its own active SQL statement. Cached navigation snapshots and byte-addressed log portions support offline browsing and the `logs search` JSON contract.

The browser remains mounted behind the pipeline inspector. `Inspector` owns the execution tree and reading state; `View` owns persistent, virtualized Ratatui list viewports for browser panes, inspector panes, and pickers. Spatial navigation and 250 ms previews use shared transitions, with context checks and scheduler epochs rejecting obsolete responses. Cache snapshots can arrive before HTTP results without taking keyboard focus. Only the top interactive layer receives input; list paging uses its actual rendered height.

Terminal state that changes together is one enum rather than parallel flags. Focus determines the screen. `Notice` carries its own tone. `ProjectScope` selects all, favorite, or project repositories. `FindPhase` follows a finder revision from cache or a debounced request to loaded or failed. `TreeSelection` separates automatic failure selection, manual selection, and a search hit waiting for its step. `State::accept` and `State::fail` apply every result and error; the scheduler only routes them.

`pipeline_status` owns pipeline state interpretation for the TUI, `run watch`, and JSON normalization. TUI failures include `FAILED`, `ERROR`, and `EXPIRED`, with the native result still displayed. `STOPPED` remains a cancellation; it also stops `--fail-fast`. JSON retains distinct `failure`, `timed_out`, and `cancelled` conclusions. Unknown names remain visible and use the existing lowercase JSON fallback. Keep these policies here instead of comparing native strings in individual consumers; PR commit checks have their own status vocabulary.

Repository favorites are persistent configuration, scoped to the API base URL and workspace. Preference writes are serialized and survive navigation changes. Quick open and contextual filters share literal query construction with CLI list searches; query revisions reject stale results and a 250 ms debounce limits requests. An account-scoped SQLite catalog indexes repository/project metadata, imports older navigation snapshots, and supplies offline results under the cache's retention and quota policy.

This separation lets Rust callers use a client without inheriting CLI policy:

```rust
use bb_api::{Client, RepoRef, requests::CreatePullRequest};

async fn example(token: String) -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder().token(token).build()?;
    let repo: RepoRef = "acme/widgets".parse()?;
    let request = CreatePullRequest::new("Improve checks", "feature/checks".parse()?, "main".parse()?)?
        .description("Handle transport errors")
        .draft(true);
    let pr = client.create_pull_request(&repo, &request).await?;
    println!("{}", pr.id);
    Ok(())
}
```

## Failure and safety boundaries

- `Error` is the library's canonical concrete error, with private categories, source, captured backtrace, HTTP status, and predicates. The application adds contextual errors without exposing token values.
- Repository, workspace, branch, PR, comment, and task identifiers validate external strings before resource construction. Path segments are percent encoded. Query filters quote external values as literals.
- API credentials remain bound to one scheme, host, port, and version prefix. Pagination validates every `next` URL and detects repeated links. It never constructs guessed page numbers. A 10,000-page cap prevents endless distinct cursors.
- The request layer disables reqwest's own retries. Only GET/HEAD connection failures, pre-body timeouts, 408/429/502/503/504 may be retried, with at most three attempts, bounded backoff/jitter, and a maximum accepted `Retry-After` of 30 seconds. Writes and partially streamed bodies are never retried.
- The streaming path alone can follow signed HTTPS S3/CloudFront redirects; it permanently removes authorization and caller headers once it leaves the API. JSON/resource pagination cannot follow those redirects.
- Default HTTP timeout is 30 seconds per attempt (`--http-timeout`), connection timeout 10 seconds. A watcher additionally bounds polls by its total deadline. Buffered JSON has a 64 MiB per-response and per-collection limit; request text has a 16 MiB limit.
- Config updates poll a nonblocking OS file lock and replace the TOML file atomically. The lock moves into the persistence worker and stays held until replacement completes, even if the awaiting task is cancelled. Unix directories are private and files use mode 0600. Tokens live in Secret Service, Keychain, or Windows Credential Manager. No plaintext credential fallback exists.
- Login, logout, and OAuth refresh share a profile lock. Refresh rereads the token under that lock before exchanging it. Login/logout acquire the configuration lock second and move both guards into one blocking worker for credential and metadata persistence. Cancellation cannot release either guard while that worker is running. The private vault interface tests these races without modifying native keyrings; native stores are selected explicitly without changing a global registry.
- `--dry-run` plans share payload construction with writes. Read-only resolution can still require a valid token. Browser login, local config changes, Git writes, remote mutations, and OAuth refresh are suppressed while planning.

The REST escape hatch is deliberately retained instead of generating hundreds of shallow commands from OpenAPI. It exposes native API features while the business commands provide stable names, validation, and agent output. Cloud endpoint details and authentication were checked against [Atlassian's reference](https://developer.atlassian.com/cloud/bitbucket/rest/) and [changelog](https://developer.atlassian.com/cloud/bitbucket/changelog/).

## Interruption and blocking work

A single Ctrl+C supervisor covers configuration loading, command execution, and output rendering. Cancellation returns exit code 2. It requests prompt cancellation and gives the async runtime up to one second for cleanup. Prompts poll terminal events and restore terminal mode with an RAII guard.

Potentially unbounded stdin reads and jaq compilation/evaluation use dedicated OS threads and one-shot result channels. Dropping a receiver abandons only local input or computation; process exit reclaims the thread. These workers never perform mutations. Synchronous keyring calls, browser launch, and atomic file replacement use Tokio's blocking pool. Async file reads keep the existing 16 MiB body and 64 KiB credential limits.

Interruption does not roll back server requests or writes already started. A blocked system call can outlive the cleanup interval until the process exits. Token rotation interrupted after the server accepts it may require login again; no local lock can make a remote exchange and keyring storage transactional.

Git discovery falls back to a context when there is no repository. A malformed Git configuration, invalid Git directory, or failure reading an existing repository is reported instead of silently choosing another repository.

## Scope and tradeoffs

The executable follows `gh` command conventions but does not claim flag-for-flag compatibility with either `gh` or the Go `bkt` client. It targets Cloud 2.0 only. It includes a terminal pipeline explorer; it does not bundle MCP, extension management, hosted OAuth infrastructure, issue tracking, or Jira automation.

Public JSON is versioned by `schemaVersion` in discovery, not by wrapping every result. Native states and IDs remain available alongside normalized fields. Optional enrichments are explicit to control API cost. The low-level `api` command provides an escape hatch when Cloud evolves.

Platform release jobs test native builds on each supported architecture. Tests in a Linux development workspace cannot establish that a live account, local browser, Windows keyring, macOS Keychain, or signed artifact works on another machine. Those remain environment-specific acceptance checks, with no mock result presented as live validation.
