# Contributing

Bug reports and focused pull requests are welcome. For vulnerabilities, follow [SECURITY.md](SECURITY.md).

Include `bb --version`, operating system, the command and expected behavior, and a minimal redacted reproduction. Do not attach tokens, private repository data, full environment dumps, or cached logs containing credentials.

## Local development

Install Git, Python 3, and Rust via rustup. The workspace selects Rust 1.99.0. See [build prerequisites](docs/installation.md#build-from-source) for Linux and Windows.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo check -p bb-api --lib --locked
cargo doc --workspace --no-deps --locked
```

The default tests use temporary directories, mock HTTP servers, and fake credentials. They need no Bitbucket account. Python's standard library drives the native terminal checks on Unix and Windows. The explicitly ignored Windows keyring test writes and removes a temporary fixture credential only when invoked separately.

## Changes to behavior

Preserve documented CLI arguments, JSON shapes, exit codes, redaction, dry-run behavior, and existing credential/cache formats. Update command discovery, relevant user documentation, and CHANGELOG.md together. Add regression coverage for observable behavior that could break; avoid tests coupled only to implementation details.

Keep platform-specific code behind narrow boundaries and use native CI to validate it. Follow the [Rust conventions and lint policy](docs/development.md), applying the Microsoft guidelines to this CLI/library workspace rather than adding abstractions solely to satisfy a checklist.

The official skill lives in `crates/bb-cli/skills/bb-cli/SKILL.md`. Keep it short, scoped to Bitbucket Cloud, and consistent with actual commands. Do not include credentials, scripts to fetch unreviewed instructions, or assumed authority to mutate remote resources.

Explain the user-visible change and validation in your pull request. Keep independent changes in reviewable commits. Release and publication steps are documented [separately](docs/releasing.md).
