---
name: bb-cli
description: Use bb to work with Bitbucket Cloud repositories, pull requests, pipeline runs, and cached logs. Use when the task involves Bitbucket Cloud or bb commands; discover available operations and schemas from the installed binary.
license: MIT
---

# Bitbucket Cloud with bb

Requires `bb` on PATH. Remote operations need Bitbucket credentials and access;
schema discovery and skill installation work offline.

Use the installed `bb` command for the requested Bitbucket Cloud workflow. Inspect
`bb --version`, then `bb schema COMMAND` for the command's arguments, effects,
required scopes, and output schema. Discovery needs no credentials or network.
Read only the relevant command schema; the root schema describes every command.

## Choose context and output

- Supply `-R WORKSPACE/REPO` when the repository is known. Use `--workspace` for
  workspace operations. Do not guess when several remotes or accounts match.
- Use `--non-interactive` and select the JSON fields needed for the task.
  Bare `--json` lists fields without executing, including on mutation commands.
  `--json '*'` executes and selects all fields; it can request extra API reads.
- Parse stdout as the result and stderr as diagnostics or a JSON error. Check
  both the exit code and the returned data. Avoid matching English error text.
- Use ordinary CLI commands for automation. `bb tui` requires a terminal and
  rejects JSON output. `--jq` runs an embedded JSON filter, not a shell.

## Credentials

Use an existing authenticated profile or the environment's `BB_TOKEN`.
Inspect `bb auth status --json '*'` or `bb auth doctor` when authentication fails.
Obtain missing credentials through the host's secret mechanism; do not put tokens
in chat, command arguments, files in the repository, or diagnostic output.
Interactive login stores credentials in the OS keyring. Headless automation can
inject `BB_TOKEN`; there is no plaintext credential fallback.

## Pull requests

```sh
bb pr list -R acme/widgets --non-interactive --json number,title,headRefName
bb schema pr view
bb pr view 42 -R acme/widgets --non-interactive --json number,title,body,files,checks
bb schema pr create
bb pr create -R acme/widgets --head feature/checks --base main --title "Improve checks" --body-file description.md --dry-run --json '*'
```

Replace example repositories and IDs with the task's actual values. Put multiline
descriptions and comments in a UTF-8 file and use `--body-file`; preserve literal
text instead of interpolating it through a shell. Add `--yes` only when the user
has authorized the destructive operation it confirms. A skill does not grant
permission to publish, merge, delete, or communicate beyond the user's request.

## Pipelines and logs

```sh
bb run list -R acme/widgets --non-interactive --json number,status,conclusion
bb run view 42 -R acme/widgets --non-interactive --json number,status,conclusion,steps
bb run watch 42 -R acme/widgets --timeout 300 --non-interactive --json number,status,conclusion
bb run view 42 -R acme/widgets --log --step Build
bb logs search 'compilation failed' -R acme/widgets --json runNumber,stepName,text,logComplete
```

`conclusion` distinguishes `failure`, `timed_out`, and `cancelled`; preserve this
distinction when reporting. Log search covers only downloaded portions of the
local cache. An empty result cannot establish that an uncached log has no errors.

## Mutations and uncertain outcomes

Preview writes with `--dry-run --json '*'` when a plan is useful. Dry runs can
perform reads to resolve IDs, but do not change remote or local state. Execute
only within the user's authorized scope.

Exit codes: `0` success; `1` invalid input, API error, or failed execution;
`2` interrupted/cancelled; `4` authentication required; `8` still pending at the
deadline. Watch commands may return useful JSON with exit `1` or `8`.

A timed-out, interrupted, or failed write may already have changed Bitbucket.
Inspect the resulting PR, pipeline, or other resource before retrying. Never
blindly repeat a merge, pipeline trigger, or other mutation. Cancelling a watcher
does not cancel the remote operation. Use `bb api` only when the normal commands
do not expose the needed operation, after inspecting `bb schema api`.
