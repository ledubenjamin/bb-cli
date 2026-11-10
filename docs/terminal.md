# Terminal explorer

Start with `bb tui`, optionally selecting `--workspace WORKSPACE` or `-R WORKSPACE/REPOSITORY`. The interface and keyboard help are English. Explore shows projects, repositories, and pipelines. Opening a pipeline displays a **Pipeline inspector** above the dimmed browser, with **Builds → Steps → Logs**. At 120 columns or wider, all three inspector panes are visible; smaller terminals show the focused pane with persistent tabs. `NO_COLOR` preserves the focus border, cursor, and status labels without color.

The active pane has a strong border and filled selection; other panes retain a subtle selection marker. Cyan identifies Explore, purple identifies the inspector, and status colors distinguish successful, failed, and running executions. `Updated 21s ago` and `Cached 3m ago` describe data freshness, not execution duration.

## Workspace selection

Save a default with `bb config set default_workspace acme`, inspect it with `bb config get default_workspace`, or press `w` in the interface. The picker offers **Use for this session** and **Set as default**. Saving uses the existing atomic configuration writer. No organization is compiled into the executable.

Resolution order:

1. `--workspace` / `BB_WORKSPACE`.
2. Explicit `-R` / `BB_REPO`.
3. The selected context's workspace, then its repository's workspace.
4. The saved `default_workspace` preference.
5. The workspace found through Git repository discovery.

If no workspace resolves, the picker opens automatically. It lists accessible workspaces through the public `/user/workspaces` endpoint. Type an exact slug when the token cannot enumerate workspaces. Listing more workspaces uses `PgDn`; changing the workspace cancels obsolete requests.

## Navigation

When no repository resolves from `-R`, the environment, a context, or Git, the explorer starts in **Favorites** if the workspace has any. Otherwise it opens Projects. **Favorites** and **All repositories** remain at the top of the unfiltered project list.

| Key | Action |
| --- | --- |
| Up / down, `j` / `k` | Select and preview projects, repositories, builds, or steps; scroll log text |
| Left / right, `h` / `l` | Previous / next pane, without wrapping; right on a pipeline opens the inspector |
| `Enter` | Open the next pane or inspector; expand/collapse a group, or read a step's logs |
| `Space` in Steps | Expand/collapse a group, or read a step's logs |
| `Tab` / `Shift+Tab` | Cycle panes inside the active browser or inspector |
| `1` / `2` / `3` | Select the corresponding pane |
| `Esc` | Close the top dialog or zoom first; close the inspector and return to Pipelines; clear an active panel filter |
| `*` | Add/remove the selected repository, or the current repository in Inspect, from Favorites |
| `Ctrl+P` | Quick open: find a repository or project across the current workspace |
| `F` | Cycle All, Failed, and Running filters over loaded runs |
| `m` | Load the next 50 projects, repositories, or runs |
| `e` / `E` | Next / previous failed step |
| `/` | Find projects/repositories in the focused panel; search logs in Pipelines or Inspect |
| `L` | Search cached logs from any pane |
| `n` / `N` | Next / previous current-step search match |
| `PgUp` / `PgDn` | Move by the visible pane height; load another log portion at a window boundary |
| `Home` / `End` | First / last list entry, or beginning / tail of the log |
| `Shift+Left` / `Shift+Right` in Logs | Scroll long lines horizontally |
| `f` | Toggle following live logs |
| `z` | Zoom the log pane |
| `w` | Choose workspace or change the default |
| `r` | Retry / refresh the current selection |
| `?` | Keyboard help |
| `q` / Ctrl+C | Quit and restore the terminal |

Runs are shown newest first. Filters describe the **loaded** run set; `m` discovers older matching runs. Failed means Bitbucket reports `FAILED`, `ERROR`, or `EXPIRED` (timed out), independently of text found in a log. This same rule drives the failure count, `!` marker, automatic step selection, and `e`/`E` navigation. The native result stays visible so a timeout can be distinguished from another failure. `STOPPED` is a cancellation and is excluded from these failure actions.

Projects and repositories preview the next column after 250 ms without movement, keeping keyboard focus in the current column. Right or Enter proceeds immediately. In the inspector, moving through Builds previews a different execution without closing the window; moving through Steps previews its logs. Enter or Space expands children, while left/right always changes panes. The first failed step is selected automatically until you choose a step yourself. Escape returns to the last inspected build, retaining the browser context, filters, and list positions.

Lists never scroll when all entries fit. Longer lists scroll only at the viewport edge; resizing and filtering keep the selection visible. Log up/down scrolls the actual text one line at a time and pauses Follow without jumping. Reading positions are remembered per step during the session. Selecting a group shows its context instead of leaving an unrelated step's logs on screen.

## Favorites and finding repositories

Press `*` on a repository to save it in Favorites. From a pipeline or log, the shortcut saves the current repository. Favorites are alphabetical and grouped by workspace and API base URL. They are local preferences shared between profiles in the same configuration; changing a token or clearing the log cache does not remove them. A missing or inaccessible repository remains bookmarked so it can be removed explicitly.

`Ctrl+P` opens **Quick open** from any screen. An empty repository search shows Favorites. Type part of a name or identifier; `Tab` switches between **Repositories** and **Projects**, arrows select, `Enter` opens, `PgDn` loads more results, and `Esc` closes without changing the current navigation. `*` toggles a repository favorite inside the picker.

`/` in Projects searches that workspace's projects. In Repositories it searches all repositories of the open project, or the workspace when **All repositories** is selected. In Favorites it searches saved repositories. `Enter` keeps the filter and exits text entry; `Esc` clears it. While entering text, letters are query characters rather than navigation shortcuts.

Matching is literal and case-insensitive. Repository searches include name, slug, full reference, project key, and project name; project searches include name and key. Cached matches appear first. Online searches run after 250 ms without typing and query Bitbucket, including resources not yet loaded in the panel. Results arrive in pages of 50, with a 10,000-item display bound; refine the query for larger sets. **Cached results only** identifies offline results or cached fallback after a failed request. An empty cache is not evidence that the workspace has no matching resources.

CLI equivalents:

```sh
bb repo favorite add acme/widgets
bb repo favorite list --workspace acme --json repository,workspace
bb repo favorite remove acme/widgets --dry-run --json '*'
bb repo list --workspace acme --search widgets --project APP --json nameWithOwner,project
bb project list --workspace acme --search application --json key,name
```

Favorite commands do not access HTTP or the keyring. Add/remove are idempotent and support `--dry-run`. Repository/project list searches use the API; `--limit` counts matching results across pages. The CLI's existing JSON field selection and embedded `--jq` apply.

## Children and attempts

The public steps endpoint does not consistently embed child execution objects. The explorer therefore indexes child runs using `trigger_type=PARENT_STEP` and joins their explicit `trigger.metadata.pipeline_uuid`, `step_uuid`, and `pipeline_run_uuid` references. Links returned for Bitbucket's internal web API are not followed.

Opening a child directly also resolves its parent chain. A triggering step whose UUID disappeared from the current attempt is retrieved individually and shown under **Earlier triggers**. Matching by step name would conflate attempts, so names are used only for display. Child runs and steps display their own attempt number. Cyclic references and excessive nesting are bounded.

The first failed leaf is selected automatically once the relevant execution data is available. Manual selection takes priority over subsequent refreshes. Expanding a child loads its steps on demand; the tree has no ten-step limit. Cached search results retain repository, root/parent execution, pipeline and step attempts, and an execution path.

## Downloads, search, and offline mode

The first log request asks for the last 64 KiB. Existing SQLite snapshots appear while online requests refresh builds, steps, and logs. Older and newer log portions are downloaded when needed. Live logs poll every two seconds; execution status polls every five seconds. Follow retrieves a final tail after observing that a step has finished, retrying if that download fails. Pausing Follow defers this read until it resumes. At most four jobs run concurrently. Navigating cancels obsolete HTTP work, and result tickets prevent late responses from replacing a newer selection. Preference writes are serialized independently of navigation; normal `q` exit finishes pending saves. Stored OAuth credentials refresh through the CLI's existing rotation lock before expiry.

`206` responses retain absolute offsets and remote total length. `304` preserves cached content; `416` can signal the end or trigger a fresh tail after truncation. If the server ignores Range and returns `200`, the body streams into the cache and automatic incremental polling is disabled for that selection. Press `r` to reload. One download is bounded to 64 MiB. A display window contains at most 2,000 lines and 2 MiB of text; larger logs remain accessible through successive windows and cache search.

Raw portions are retained separately from sanitized display text. Overlaps are merged transactionally; partial lines and split UTF-8 characters are reindexed when adjacent bytes arrive. Terminal control sequences are removed from display/search text. Search offsets still refer to the original bytes. Very long lines use overlapping search fragments so a supported query can cross a fragment boundary without duplicate hits.

`bb logs search QUERY` accepts a literal, case-insensitive string of 3–1,024 characters. Quotes and FTS operators are treated as text. The default result limit is 30; `--limit` accepts 1–1,000. `-R` narrows the repository; `--workspace` or normal workspace resolution chooses the workspace. Search and `bb tui --offline` do not authenticate or issue HTTP requests.

Search and highlighting use the same Unicode matching rules. Accents are significant; composed and decomposed spellings are not normalized. Changing the search text or scope discards older results. Horizontal scrolling and input cursors count terminal columns, keeping wide characters and combining sequences intact.

Only downloaded portions are searchable. `logComplete: false` means that missing text can be outside the cache. `updatedAt` is a Unix timestamp of the last download. `byteOffset` identifies the match in the raw source, including any preceding escape bytes. Search results also include `repository`, `rootRunUuid`, `parentRunUuid`, `parentRunAttemptUuid`, `parentStepUuid`, `runUuid`, `runNumber` (Bitbucket build number), `runAttempt`, `stepUuid`, `stepName`, `stepAttempt`, and `executionPath`. Use `bb logs search --json` to discover the complete schema.

## Local storage

The cache is `logs.sqlite3` in the platform's `bb` cache directory, usually `~/.cache/bb` on Linux. With `BB_CONFIG_DIR`, it lives in `BB_CONFIG_DIR/cache`. SQLite with FTS5 trigram search is bundled; no SQLite service, sqlite3 executable, or external jq executable is required.

Cache identity includes the configuration namespace, API origin, selected profile and account UUID, or a SHA-256 fingerprint when using `BB_TOKEN`. Tokens are never stored in SQLite. Offline searches must use the same identity as the downloads; changing accounts does not expose the previous account's logs. The files contain pipeline output, which can include data emitted by builds. Unix cache directories use mode 0700 and the database uses 0600.

Seven-day retention is measured from download, not from reading or searching. The 500 MiB budget includes the database, indexes, and WAL; part of the budget is reserved for write and maintenance headroom. Expired data and then the oldest cached logs are evicted. A short OS file lock serializes maintenance and writes across processes, protecting the log being extended. A dedicated thread owns SQLite, with a bounded request channel and cancellation tied to the running request. Busy readers or an exhausted quota can pause caching without blocking terminal input.

Repository and project metadata use a searchable SQLite catalog under the same account isolation, retention, and storage budget. Existing navigation snapshots are imported into the catalog without changing their original timestamps. Favorite references live separately in `config.toml`, have no expiration, and are available even when no metadata is cached.

`bb logs clear --workspace acme` removes that identity's cached logs for the workspace. Navigation metadata and Favorites remain available. `--dry-run --json '*'` previews this local operation. The TUI itself rejects `--dry-run`, `--json`, `--jq`, `--verbose`, and `--non-interactive`; its offline command metadata remains available through `bb schema tui`.

For full online browsing, API tokens need `read:user:bitbucket`, `read:workspace:bitbucket`, `read:project:bitbucket`, `read:repository:bitbucket`, and `read:pipeline:bitbucket`, plus access to the workspace/repositories. OAuth uses the corresponding account, project, repository, and pipeline scopes. See `bb schema tui` for the command contract.
