# Using bb

## Authenticate

For agents and CI, inject `BB_TOKEN` from your secret manager. Tokens are bearer credentials, scoped to Bitbucket's API origin.

```sh
# Interactive, masked token prompt; stored in the operating system keyring:
bb auth login --profile work

# Read a token from a protected file, without putting it in argv:
bb auth login --profile work --with-token < /secure/bitbucket-token

# Repository/workspace/project access token:
bb auth login --profile automation --token-type access --with-token \
  -R acme/widgets < /secure/resource-token

bb auth status --json '*'
bb auth doctor
bb auth switch work
bb auth setup-git
```

`BB_TOKEN` takes precedence over saved credentials. `BB_TOKEN_TYPE` is `api` by default; use `access` for resource access tokens or `oauth` for externally supplied OAuth tokens. API-token login/status uses `/user` and needs `read:user:bitbucket`; resource tokens validate `-R` instead. Each command's schema reports its API-token and OAuth scopes. Scopes alone do not grant repository membership or administrator privileges.

For browser authorization, create **your own confidential Bitbucket OAuth consumer** with callback `http://127.0.0.1:8765/callback`. Supply its key through `BB_OAUTH_CLIENT_ID` or `bb config set oauth_client_id KEY`, and inject `BB_OAUTH_CLIENT_SECRET` from your secret manager:

```sh
bb auth login --web --profile work
bb auth refresh
```

The browser flow requires a terminal, validates OAuth `state`, and times out after five minutes. Access and refresh tokens live in the OS keyring. A shared profile lock serializes login, logout, and token rotation across concurrent `bb` processes. Keep the consumer secret available for later refreshes. `BB_OAUTH_REDIRECT_URI` overrides the callback; only literal loopback HTTP addresses are accepted. No shared consumer or hosted token broker is bundled.

`auth logout` removes the local credential; it does not revoke a token in Bitbucket. Configuration stores profile metadata and preferences, never tokens. A missing or locked keyring does not fall back to plaintext credentials.

## Select accounts and repositories

```sh
bb context create work --profile work --workspace acme -R acme/widgets
bb context use work
bb context list --json '*'
bb repo set-default acme/widgets
bb config set git_protocol ssh
bb config set default_workspace acme
bb config get default_workspace
```

Repository precedence: `-R` / `BB_REPO`, `BITBUCKET_REPO_FULL_NAME`, local Git `bb.repository`, one unambiguous Bitbucket remote, then the selected context. Multiple distinct Bitbucket remotes require `-R` or `repo set-default`. HTTPS, SSH, and `workspace/repository` inputs are accepted. A PR URL may select its repository; conflicting `-R` input is rejected.

`--workspace` / `BB_WORKSPACE` selects workspace operations. `--context` / `BB_CONTEXT` overrides the active context, including the credential helper used by Git subprocesses. `BB_CONFIG_DIR` overrides the platform configuration directory, including its isolated keyring namespace.

Workspace precedence is `--workspace` / `BB_WORKSPACE`, explicit `-R` / `BB_REPO`, the selected context's workspace or repository, `default_workspace`, then Git repository discovery. The terminal's workspace picker can override the workspace for one session or save a new default.

## Terminal explorer and cached logs

```sh
bb tui
bb tui -R acme/widgets
bb tui --offline
bb logs search 'compilation failed' --workspace acme \
  --json repository,runNumber,stepName,text
bb logs search 'timeout' -R acme/widgets --json '*' --jq 'map({runNumber,stepName,byteOffset})'
bb logs clear --workspace acme
```

The English-only Ratatui interface explores projects, repositories, and pipelines with automatic previews and a clearly highlighted active pane. Save repositories with `*`; Favorites open first when no repository is targeted. `Ctrl+P` finds repositories or projects across the workspace, and `/` searches the focused project/repository list. Opening a run displays a **Builds → Steps → Logs** inspector above the dimmed browser. Use left/right to change panes and up/down to preview builds or steps and scroll logs. Enter/Space expands children; original triggers from earlier attempts appear under **Earlier triggers**. `Esc` returns to the last inspected build. Use `Tab` or `1`–`3` to select panes, `e` / `E` for failed steps, `L` to search logs, `f` to follow, `z` to zoom, `w` to switch workspace, and `?` for help.

Scripts can use `bb repo favorite add acme/widgets`, `bb repo favorite list --workspace acme --json repository`, `bb repo list --search widgets --project APP`, and `bb project list --search application`. Favorites persist in local configuration; online list searches include matching resources beyond the first page.

Logs initially load their last 64 KiB. Scrolling can fetch older portions. A private SQLite cache shares downloaded logs with `bb logs search`, which runs without HTTP or keyring access. The cache retains data for seven days since download and has a 500 MiB storage budget including indexes and WAL. Search is literal, case-insensitive, and limited to downloaded portions; each match reports whether its source log is complete. SQLite and the search tokenizer are bundled, and `--jq` uses the existing embedded jaq engine.

See the [terminal guide](terminal.md) for navigation, account isolation, offline behavior, and download limits. The TUI requires terminal stdin/stdout and rejects machine-output flags; agents should use `bb run`, `bb logs search`, and `bb schema`.

## Commands

| Group | Operations |
| --- | --- |
| `auth` | login, status, doctor, switch, refresh, logout, setup-git |
| `context`, `config` | Named contexts and non-secret preferences |
| `repo` | list, view, create, edit, delete, clone, set-default, default-reviewers list |
| `pr` | list, view, create, edit, checkout, diff, close, reopen, ready, merge, review, comment, status, checks |
| `pr comments` | list, delete, resolve, reopen |
| `pr task` | list, create, complete, reopen |
| `branch` | list, create, delete, set-default, rebase |
| `branch protect` | list, view, add, edit, remove |
| `commit` | diff, status |
| `project` | list, view, create, edit, delete |
| `perms repo`, `perms project KEY` | list, grant, revoke, for users or groups |
| `run` | list, view, watch; `view --log` streams step logs |
| `tui` | Favorites, quick search, projects, repositories, pipeline trees, and logs; `--offline` uses the cache |
| `logs` | search locally downloaded logs, clear a workspace's cached logs |
| `workflow` | run default/custom pipelines on branches or tags |
| `variable` | list, get, set, delete at repository/workspace/deployment scope |
| `webhook` | list, create, delete |
| `status`, `browse`, `api` | Personal PR overview, browser navigation, native REST |
| `skills` | Show, install, inspect, and remove the bundled official agent skill |
| `schema`, `completion` | Offline machine discovery and shell completion |

Run `bb COMMAND --help` or `bb schema COMMAND` for exact arguments. PR targets accept numbers, branch names, or Bitbucket URLs; omission finds the current branch's single open PR. Reviewers are UUIDs or Atlassian account IDs.

```sh
bb pr review 42 -R acme/widgets --approve
bb pr comment 42 -R acme/widgets --body-file review.md --path src/lib.rs --line 12
bb pr comments resolve 42 19 -R acme/widgets
bb pr task create 42 -R acme/widgets --body "Cover the timeout path"
bb pr checks 42 -R acme/widgets --watch --timeout 300 --json key,state
bb pr merge 42 -R acme/widgets --squash --delete-branch --yes --json number,state

bb branch protect add -R acme/widgets --pattern main \
  --kind require-approvals-to-merge --value 2 --dry-run --json '*'
bb project create OPS --workspace acme --name Operations
bb perms project OPS grant --workspace acme --group developers --permission write

bb workflow run deploy -R acme/widgets --ref main --watch --timeout 600
bb run view 42 -R acme/widgets --json number,status,conclusion,steps
bb run view 42 -R acme/widgets --log --step Build
bb variable set API_SECRET -R acme/widgets --secured --body-file /secure/api-secret
bb variable set --scope workspace --workspace acme --env-file settings.env
bb variable list -R acme/widgets --deployment production --json key,secured
```

`run watch` always waits; `workflow run --watch` and checks' `--watch` opt into waiting. A timeout returns the latest available state with exit code 8. `--fail-fast` stops waiting after a failed check or pipeline step. A merge waits for asynchronous task completion and verifies the PR is actually `MERGED`; timeout or cancellation does not imply the server cancelled it.

Git checkout/rebase require a clean working tree and a matching Bitbucket remote. Checkout fetches forks without embedding tokens in URLs, creates `pr/NUMBER` by default, and refuses to overwrite an existing branch. Rebase leaves conflicts for `git rebase --continue` / `--abort` and never pushes. The CLI never force-pushes.

Variable updates preserve an existing variable's secured flag unless `--secured=true|false` is explicit. Publishing a previously secured value requires confirmation. Secured values are redacted from structured output and dry-run plans. Env files support literal `KEY=VALUE`, optional `export`, single/double quotes, and comments; no shell interpolation, escape evaluation, or multiline values. Duplicate keys fail before writes. Batch updates are sequential; errors report how many writes completed, with no automatic rollback or retry.

## JSON and automation

```sh
bb schema pr merge
bb pr list --json                 # Discover fields offline; no action executes
bb pr list -R acme/widgets --json number,title
bb pr list -R acme/widgets --json number,title --jq '.[] | "#\(.number) \(.title)"'
bb api 'repositories/{workspace}/{repo}/pullrequests' -R acme/widgets \
  --paginate --slurp --jq '[.[].values[].id]'
```

`--json '*'` selects all documented fields. `pr view` loads comments/files/tasks/checks only when selected; `run view` similarly loads steps. On list and mutation results those enrichment fields are `null`. Prefer explicit selections to avoid extra API calls. `--jq` is embedded through jaq and never launches a process; its JSON subset is documented in the [agent contract](agents.md).

Results go to stdout; progress and errors go to stderr. With `--json`, errors are JSON objects. Empty lists are `[]`. No colors or progress are added to structured stdout. Set `BB_PROMPT_DISABLED=true` or `--non-interactive` for automation; a non-terminal stdin also disables confirmation prompts. Destructive deletion/revocation operations require `--yes`.

`--dry-run` emits a redacted action plan without writing to Bitbucket, Git, configuration, skills, or the keyring. Reads may resolve defaults, IDs, reviewers, or deployment environments. Expired stored credentials must be refreshed separately before such a plan. No interactive editor or implicit title/body generation is used.

| Exit | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Invalid input, API/transport error, failed checks or pipeline |
| `2` | Cancelled/interrupted |
| `4` | Authentication required or unusable credentials |
| `8` | Checks/pipeline/merge still pending at the deadline |

## REST escape hatch

`bb api PATH` defaults to GET, or POST when a body is supplied. `-F key=value` parses JSON literals; `-f key=value` forces a string. With explicit GET/HEAD, these fields become query parameters. `-P key=value` always adds a query parameter. `--input FILE` reads a JSON body (`-` reads stdin). `-H 'Name: value'` sets ordinary headers; authentication, cookie, and Host headers are reserved.

`--paginate` follows server-provided `next` links for GET only. Without `--slurp` it prints one JSON page per line; `--slurp` returns an array of pages. `--json` / `--jq` also processes the complete page array. Responses keep native Bitbucket names; `--include` prints headers and cannot be combined with structured filtering. A successful empty response is JSON `null`.

Authenticated URLs and pagination stay on `https://api.bitbucket.org/2.0/`. Streamed downloads additionally accept signed HTTPS S3/CloudFront redirects with all credentials removed. `BB_API_URL` is reserved for numeric loopback test servers. JSON responses and accumulated paginated collections are limited to 64 MiB; diffs and logs stream without that buffering limit. Request bodies are limited to 16 MiB. HTTP timeouts and polling intervals are bounded to one day; finite watch timeouts to one year (0 means unbounded). Transient reads have at most three attempts; writes are never automatically retried.

