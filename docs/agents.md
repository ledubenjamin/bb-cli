# Agent contract, version 1

Start with `bb schema COMMAND`. It works without a token, Git repository, configuration file, or network. The root schema recursively describes all public commands. It is derived from the same clap command tree as `--help` and completion.

Each command includes arguments, possible values, defaults, conflicts, effect, scopes, JSON fields, a JSON Schema for results, optional output variants, and a dry-run schema. `apiRequestSchema`, when present, describes the native REST body. `scopeNotes` identifies optional reads needing additional scopes. An effect is `read`, `remote-write`, `local-write`, or `dynamic` (`api`). Required argument flags are parser requirements; semantic combinations are further validated before execution.

1. Discover: `bb schema pr create` or `bb pr create --json`.
2. Inspect: `bb pr list -R acme/widgets --json number,title,headRefName`.
3. Plan: `bb pr create -R acme/widgets --head feature --base main --title Fix --dry-run --json '*'`.
4. Execute the chosen command with explicit arguments. Add `--yes` only for an intended destructive action.
5. Check the exit code and parse stdout or the JSON error on stderr.

Bare `--json` is field discovery, **including on mutation commands**; it does not execute the command. A field list or `'*'` requests execution. Place `--json` after the command, as in `gh`. Invalid field names and invalid jq syntax are rejected before action execution. `tui` is an interactive-only exception: it rejects JSON and dry-run flags. Inspect it with `bb schema tui`; its metadata has `interactive: true`.

`bb logs search QUERY --workspace acme --json repository,runNumber,stepName,text` searches the local SQLite cache without HTTP or keyring access. Results identify the root/parent pipeline, the pipeline and step attempts, and the exact source `byteOffset`. `logComplete` and `updatedAt` describe coverage and download freshness; an empty search cannot establish that an uncached log contains no error. Search uses the current configuration/profile/account namespace or `BB_TOKEN` fingerprint. See the [terminal guide](terminal.md) for coverage and retention limits.

`bb repo list --workspace acme --search TEXT --project KEY` and `bb project list --workspace acme --search TEXT` query Bitbucket using literal, case-insensitive matching; `--limit` applies after filtering across pages. Repository matching includes its project name/key. Existing `--json` and `--jq` work unchanged.

`bb repo favorite add WORKSPACE/REPO`, `remove WORKSPACE/REPO`, and `list` manage local preferences without HTTP or keyring access. List uses the selected workspace and exposes `repository`, `workspace`, and `apiOrigin`. Add/remove are idempotent, return the normal Action contract, and support `--dry-run`; their effect is `local-write` and their required scopes are empty. Favorites are shared between profiles in the same configuration, separated by API base URL, and do not expire with cached logs.

## Result conventions

`bb skills show`, `install --agent codex|claude`, `status`, and `remove` manage only the official instructions bundled in the executable. They run without loading Bitbucket configuration or credentials. Installation and removal are idempotent local writes, support dry runs and JSON discovery, and refuse to overwrite local edits. Status returns the destination, versions, and installation state. See [agent skills](skills.md) for scope selection and the conflict behavior.

PRs expose `number`, `title`, `body`, `state`, `nativeState`, `isDraft`, `headRefName`, `baseRefName`, `headRefOid`, `headRepository`, `baseRepository`, `author`, `reviewers`, `participants`, `url`, `createdAt`, `updatedAt`, and optional `comments`, `files`, `tasks`, `checks`.

`OPEN` and `MERGED` match their native state. Native `DECLINED` and `SUPERSEDED` become `CLOSED`; `nativeState` preserves their distinction. A future unrecognized state becomes `UNKNOWN`, never a successful merge.

Runs expose `uuid`, `number`, `status`, `conclusion`, `nativeState`, `headBranch`, `headSha`, `createdAt`, `completedAt`, `url`, `steps`. Typical statuses are `queued`, `in_progress`, `completed`; conclusions are `success`, `failure`, `cancelled`, `timed_out`, or `null`. Native states remain available.

Repositories use `nameWithOwner`, `isPrivate`, `defaultBranch`. Other resources use camelCase field names. Native identities are retained. Permission lists describe explicit assignments at the selected resource, not effective/inherited access.

Unselected enrichments are `null`; selected empty enrichments are `[]`. Full enrichments are provided by `pr view` / `run view`, not list or mutation responses. A normal list has a total item limit (`--limit`, default 30), across all API pages. A dry-run returns an action envelope instead of a resource, so select `--json '*'` or action fields when planning.

```json
{"operation":"pr create","status":"planned","dryRun":true,"actions":[{"method":"POST","path":"repositories/acme/widgets/pullrequests","body":{"title":"Fix","source":{"branch":{"name":"feature"}},"destination":{"branch":{"name":"main"}},"draft":false,"close_source_branch":false}}]}
```

Action bodies retain REST names. Credentials and secured variable values are redacted. An action result with `dryRun:false` confirms that its request completed; no fabricated deleted resource is returned for HTTP 204.

## Error handling

```json
{"error":{"code":"authentication_required","message":"run bb auth login or set BB_TOKEN"},"exitCode":4}
```

Common error codes: `invalid_arguments`, `authentication_required`, `permission_denied`, `not_found`, `rate_limited`, `transport_error`, `api_error`, `pending`, `cancelled`, and `error`. Use the numeric exit code as the stable process contract; do not match English error text.

A failed or timed-out write can already have changed the server. Inspect remote state before retrying. `bb` does not automatically repeat POST/PUT/PATCH/DELETE, including pipeline triggers and merges. Cancellation does not roll back completed batch operations or cancel an accepted remote merge/pipeline. Ctrl+C also interrupts startup, stdin reads, lock contention, terminal prompts, and output filtering. It returns exit 2, with a JSON error on stderr when requested. Local cleanup has a one-second budget; stdout may be incomplete if interruption occurs during rendering.

`checks` can return populated JSON with exit 1 or 8. A pipeline watcher similarly returns its last known run. Empty/unknown checks are pending, not successful. `--fail-fast` can return failure while other checks/steps continue. The merge command's timeout reports a pending error; its server task may still finish later.

Use `BB_TOKEN` in CI. Git and mutation confirmations never prompt when stdin is not a terminal. HTTPS Git credentials are supplied through a host-scoped helper, not remote URLs or process arguments. SSH uses the user's keys and agent in batch mode; explicitly configured SSH commands remain under the user's control.

## jq

Examples supported by the embedded jaq engine:

```sh
bb pr list -R acme/widgets --json number,title --jq 'map({number,title})'
bb pr list -R acme/widgets --json number,isDraft --jq '.[] | select(.isDraft == false) | .number'
bb run list -R acme/widgets --json number,conclusion --jq '[.[] | select(.conclusion == "failure")]'
bb api user --jq '.uuid'
```

Filters support JSON traversal, selection, mapping, reduction, arithmetic, regular expressions, and string formatting from jaq's core/standard/JSON libraries. This is not an external jq process: no shell execution, module-file imports, environment access, process-exit filters (`halt`/`halt_error`), or jq CLI flags are provided. A filter may emit at most 100,000 results. Evaluation has no automatic CPU deadline; Ctrl+C interrupts the command even while a filter computes without producing a result. Strings are emitted raw, one result per line; other filter results are serialized as JSON. Outside `api`, `--jq` requires `--json`.

Diffs and logs are raw streamed bytes unless JSON is requested, in which case the result is a UTF-8 `{"text":"..."}` object. `bb api` keeps native REST JSON, and `--paginate --slurp` returns page objects rather than flattening `values`.
