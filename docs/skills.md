# Official agent skill

`bb skills` distributes the official instructions for using this installed version of `bb`. It works offline, without a token, a configured Bitbucket account, or a working Bitbucket configuration file. It does not fetch or execute third-party skills.

## Install

From your project root:

```sh
bb skills install --agent codex
bb skills install --agent claude
```

Choose one command for the agent you use, or both if your project uses both. `--agent` is required; installation does not guess from whichever tools happen to be on PATH. The default scope is the current directory, not an inferred Git root.

| Agent | Project scope (default) | User scope (`--scope user`) |
| --- | --- | --- |
| Codex | `.agents/skills/bb-cli/SKILL.md` | `~/.agents/skills/bb-cli/SKILL.md` |
| Claude Code | `.claude/skills/bb-cli/SKILL.md` | `~/.claude/skills/bb-cli/SKILL.md` |

`~` means the OS user home, including on Windows. These are the native directories documented by [Codex](https://developers.openai.com/codex/skills) and [Claude Code](https://code.claude.com/docs/en/skills). Reload or start a new agent session if the skill does not appear immediately. The `bb` executable must remain on the agent's PATH.

Use an explicit project directory or your user scope as needed:

```sh
bb skills install --agent codex --directory /path/to/project
bb skills install --agent claude --scope user
bb skills install --agent codex --dry-run --json '*'
```

`--directory` requires an existing directory and cannot be combined with user scope. Project installations can be committed to share instructions with collaborators. The sibling `.bb-cli.lock` file is a runtime lock, not part of the skill; ignore it in your project.

## Inspect, update, and remove

```sh
bb skills show
bb skills status --agent codex --json '*'
bb skills install --agent codex
bb skills remove --agent codex
```

After updating `bb`, re-run `install` with the same agent and scope to update its instructions. Updating the binary alone does not modify installed skills. Re-installing identical instructions is a no-op. `show` prints exactly the bundled `SKILL.md`; archives contain the same file under `skills/bb-cli/`.

`status` reports `not-installed`, `current`, `different-version`, `modified`, or `unmanaged`, together with the destination and bundled/installed versions when known. An intact managed copy from another build can be updated; a locally edited or manually installed file is never overwritten or deleted by these commands. Preserve or move that file yourself before reinstalling. There is no implicit force mode.

Only the installed `SKILL.md` is owned by `bb`; other files and the containing directories survive removal. Installation uses a temporary file in the destination directory, atomic replacement, and a process lock. Symlinks and Windows reparse points under the managed agent directory are rejected. The content digest detects local edits; it is not a cryptographic signature or proof of authorship.

## Automation

`bb schema skills install` and `bb schema skills status` describe arguments and JSON fields. `install` and `remove` have `local-write` effects and return the standard action result; `show` and `status` are reads. None requires Bitbucket scopes.

Bare `--json` discovers fields without installing. Use `--json '*'` to execute and return structured results, and `--dry-run` to preview a local change without creating directories or files. Conflicts return exit code 1 with the standard JSON error envelope when JSON output is requested.

The canonical source is [SKILL.md](https://github.com/ledubenjamin/bb-cli/blob/master/crates/bb-cli/skills/bb-cli/SKILL.md). It is compiled into the executable, so each release carries its own instructions without a separate download or mutable remote reference.
