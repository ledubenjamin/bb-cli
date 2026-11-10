# Changelog

## [0.1.3] - 2026-11-10

First release from the public repository's new history. This entry describes
the included capabilities; it is not a comparison against an earlier public tag.

### Included

- Bitbucket Cloud commands for pull requests, repositories, pipelines, logs,
  variables, permissions, and webhooks, with JSON output and offline schemas.
- An interactive terminal explorer for repositories, pipeline trees, and cached
  logs, including offline browsing and log search.
- Authentication profiles with native operating-system credential stores and
  environment-token support for automation.
- Bundled agent instructions, with `bb skills` commands to inspect, install,
  update, and remove the official skill for Codex and Claude Code.
- Native Linux x86_64 and ARM64 archives and a Windows x86_64 ZIP, with shell
  completions, schemas, documentation, and dependency notices.
- Linux and PowerShell installers with SHA-256 verification and preservation of
  an existing installation when a download or verification fails.

### Distribution

- Linux builds use an Ubuntu 22.04 baseline (glibc 2.35); the same archives are
  checked on Ubuntu 24.04 and 26.04 on both architectures.
- Windows builds and console tests run on Windows Server 2025, with installer
  checks in Windows PowerShell 5.1 and PowerShell 7.
- Release publication waits for every build and archive check, uploads assets
  into a draft, and publishes the complete stable release as Latest.

### Installation

Download the archive for your platform or follow the
[installation guide](https://github.com/ledubenjamin/bb-cli/blob/v0.1.3/docs/installation.md).
Run `bb --version` after installation; this release reports `bb 0.1.3`.
Bitbucket Server/Data Center and prebuilt macOS binaries are outside the supported
distribution matrix.
