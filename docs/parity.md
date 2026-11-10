# Cloud parity with the Go reference

Reference: `https://github.com/avivsinai/bitbucket-cli`, MIT, Aviv Sinai. The new executable is `bb`; existing `bkt` scripts need command/field migration. This matrix covers the agreed Cloud scope, not the Go client's Data Center implementation.

| Workflow | Rust entry point | Notes |
| --- | --- | --- |
| API tokens and resource tokens | `auth login`, `BB_TOKEN` | OS keyring or injected environment; typed Git usernames |
| Browser OAuth, refresh | `auth login --web`, `auth refresh` | Own consumer; loopback callback/state; locked token rotation |
| Auth diagnostics and Git helper | `auth status`, `doctor`, `setup-git` | No token display |
| Multiple accounts/contexts | `auth switch`, `context` | Explicit workspace/repository/profile association |
| Repository discovery | `-R`, environment, Git, contexts | Ambiguous remotes fail explicitly |
| Repository list/view/create/clone | `repo` | Private by default; SSH or HTTPS clone |
| Repository edit/delete/default | `repo edit`, `delete`, `set-default` | Local default separate from remote main branch |
| Default reviewers | `repo default-reviewers list` | Effective reviewers, including inheritance |
| PR list/view/create/edit | `pr` | Native lifecycle filters, fork source, reviewer add/remove |
| PR checkout/diff | `pr checkout`, `diff` | Clean-tree guard, forks, streamed bytes, `--stat` |
| PR approve/comment | `pr review`, `pr comment` | General, inline old/new, threaded, pending comments |
| Comment resolution | `pr comments` | List/filter/delete/resolve/reopen; top-level validation |
| Tasks | `pr task` | List/create/complete/reopen |
| PR decline/reopen/draft/publish | `pr close`, `reopen`, `ready --undo`, `ready` | `decline` and `publish` aliases |
| PR merge | `pr merge` | Merge/squash/fast-forward; optional branch close; waits for async task |
| PR/commit checks | `pr checks`, `commit status` | Watch, fail-fast, distinct pending/failure exit codes |
| Personal PR dashboard | `pr status`, `status` | Created-by-me and review-requested PRs in the repository |
| Branch list and local rebase | `branch list`, `rebase` | Fetch then rebase; no automatic push |
| Branch create/delete/default | `branch create`, `delete`, `set-default` | Native Cloud endpoints, default branch protection |
| Branch restrictions | `branch protect` | Native kinds, patterns, thresholds, exceptions |
| Commit comparison | `commit diff BASE HEAD` | Correct Cloud HEAD..BASE order |
| Pipeline triggers | `workflow run` | Default/custom, branch/tag, runtime variables, wait |
| Pipeline list/view/log/watch | `run` | Build number/UUID, lazy step enrichment, archived logs |
| Repository variables | `variable --scope repository` | Create/update/delete, preserves secured status |
| Workspace variables | `variable --scope workspace` | Native workspace API |
| Deployment variables | `variable --deployment NAME_OR_UUID` | Paginated environment resolution |
| Env-file imports | `variable set --env-file` | Literal parser, prevalidation, partial-failure reporting |
| Webhooks | `webhook` | List/create/delete, explicit event names |
| Projects | `project` | Cloud CRUD, added beyond the reference's Cloud subset |
| User/group permissions | `perms repo`, `perms project` | Native permission assignments and revocation |
| Raw API and pagination | `api` | Native JSON, typed/raw fields, headers, page slurping |
| JSON and jq | Global `--json`, `--jq` | Embedded filtering, camelCase business views |
| Agent discovery | `schema`, bare `--json` | Offline command/output contracts |
| Dry runs | Global `--dry-run` | Native action bodies with redaction |
| Browser/rate limits/completion | `browse`, `status rate-limit`, `completion` | Rate-limit headers reflect an actual read response |
| Data Center | Out of scope | No Cloud-to-DC fallback |
| MCP, extension/skill management | Out of scope | CLI's machine contract is the integration surface |
| Issues, Wiki, Jira | Out of scope | Cloud Issues/Wiki removal makes a legacy issue port inappropriate |

Cloud's [changelog](https://developer.atlassian.com/cloud/bitbucket/changelog/) records Issues removal in August 2026, bearer authentication for API tokens, and removal of the old `merge` query parameter for diffs in September 2026. This implementation does not send that removed diff parameter or depend on removed issue endpoints.
