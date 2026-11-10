# Development

Use Rust 1.99.0, the workspace's declared minimum and pinned development toolchain. Keep library dependency features in each consuming crate's manifest; test the library independently so CLI features cannot conceal a missing requirement.

Run the commands in [CONTRIBUTING.md](../CONTRIBUTING.md) before submitting a change. Pull requests, pushes to `master`, and manual CI runs execute strict Clippy and tests on Ubuntu 24.04, Ubuntu 26.04, and Windows Server 2025 (x86_64). Formatting, independent library checks, documentation, and dependency checks run on Ubuntu 24.04. Tags trigger the release workflow, which builds Linux x86_64/ARM64 on Ubuntu 22.04 and Windows x86_64 on Windows Server 2025. The same Linux archives must pass installation/runtime checks on Ubuntu 24.04 and 26.04 for both architectures before publication. macOS releases remain outside the supported matrix. A newer CI run cancels the previous run for the same event and ref.

CI and release jobs cache Cargo dependencies separately for each platform and save the compiled dependencies even if a test fails. Pull requests restore the CI cache populated by `master`; a first run without a matching cache still compiles from scratch. Test compilation has its own step, followed by a five-minute limit on execution. The Unix interruption tests use Python 3's standard-library PTY support. Windows terminal checks create a separate native console and exercise help, quit, Ctrl+C, and restoration. Both use bounded waits and require no additional Python packages.

## Rust conventions

Use the [Microsoft Rust guidelines](https://microsoft.github.io/rust-guidelines/agents/all.txt) when extending the workspace. Keep CLI policy outside `bb-api`, expose validated domain types, preserve useful error sources, and document public API behavior with compilable examples. Library identifiers own validated strings; borrowed parsing and owned conversions share validation without copying an already owned string.

The workspace enables the compiler recommendations, Clippy's `all`, `cargo`, and `pedantic` groups, and the recommended restriction lints. CI treats their warnings as errors. Add a narrow `#[expect(..., reason = "...")]` only where the underlying behavior warrants an exception. Prefer correcting the cause and preserve `unfulfilled_lint_expectations` so obsolete exceptions fail CI.

## Lint exceptions and build policy

- Both packages inherit the canonical repository URL from `workspace.package.repository`; crate metadata lints need no exception.
- `clippy.toml` allows three transitive duplicate families: `base64` (OAuth2 and reqwest), `syn` (procedural macros), and `windows-sys` (the optional ring backend still resolves 0.52 alongside 0.61). Recheck the list when updating dependencies.
- Independent clap flags and mutually exclusive merge strategy flags keep their documented CLI spelling. Their argument structs locally expect `struct_excessive_bools`.
- Git rejects exactly the case-sensitive `.lock` suffix in branch names; that validator locally expects `case_sensitive_file_extension_comparisons`.
- OAuth and keyring diagnostics intentionally discard selected backend errors that can contain credentials. Preserve those local `map_err_ignore` expectations and safe replacement messages; other parsing and I/O failures keep their sources.
- `literal_string_with_formatting_args` stays disabled because structured tracing templates use named properties.
- Release binaries use the portable CPU baseline for each distribution target. Server fleet CPU tuning does not apply to these general-purpose CLI archives.
- Project Rust code forbids `unsafe`. Native integration stays behind dependency APIs; the workspace does not contain an unsafe abstraction requiring a project Miri harness.

## Regression checks

Exercise observable behavior with loopback HTTP servers and isolated Git/config directories. Native keyring writes and real Bitbucket mutations do not belong in the default suite. Tests cover request validation, credential redaction, origin confinement, pagination, dry runs, JSON contracts, concurrent token refresh, lock contention, and interruption during input or CPU work.

Compile all examples with `cargo clippy --workspace --all-targets --locked -- -D warnings`. Run `cargo run -p bb-api --example prepare_pull_request --locked` for an offline payload example. The `list_pull_requests` example performs real read requests only when explicitly run with a repository argument and `BB_TOKEN`.

Install the dependency tools used by CI locally with `cargo install cargo-audit --version 0.22.1 --locked` and `cargo install cargo-machete --version 0.9.2 --locked`. CI downloads precompiled versions using the pinned `taiki-e/install-action`, with source-build fallback disabled. Update those version pins deliberately with the corresponding workflow.

Release publication has offline regression checks: run `python3 scripts/test_publish.py` on Linux. These retain coverage of old Linux-only releases. To exercise installation against a native build, package it with `python3 scripts/package.py --target YOUR_TARGET --binary PATH_TO_BB --output dist`, then run `python3 scripts/test_install.py --target YOUR_TARGET --dist dist` (use `python` on Windows). CI checks the native installer on every platform; the release workflow checks optimized archives. These checks cover first installation, replacement, paths with spaces/non-ASCII characters, schema discovery, skill installation/removal, checksum rejection, and preservation of the installed binary when a download fails. Windows runs them with PowerShell 5.1 and `--powershell pwsh` for PowerShell 7.

On Windows, explicitly run `cargo test -p bb-cli --locked native_windows_keyring_round_trip -- --ignored` to verify save/read/delete with a temporary Credential Manager entry. CI runs it separately; the default test suite never writes to the OS keyring.

Run `python3 scripts/test_package.py` (`python` on Windows) for packaging regression checks. The ZIP fixture includes an epoch-dated dependency notice: packaging clamps dates to the ZIP format's range while preserving file contents, the bundled skill, and checksum integrity.

The publication job uses GitHub's [`concurrency.queue: max`](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idconcurrency) to retain pending releases and compares stable semantic versions before moving Latest. Actionlint 1.7.12 does not recognize this documented property yet; when using that version, ignore only `unexpected key "queue" for "concurrency" section`, keeping its other checks enabled. `.github/actionlint.yaml` supplies the hosted Ubuntu 26.04 labels missing from that version's catalog.

## Maintenance and compatibility

Keep transport policy and pagination inside `bb-api`, and OAuth exchange separate from credential persistence inside `bb-cli`. Request bodies serialize directly into shared bytes once, including across read retries. Public errors remain concrete, secrets stay out of debug output, and blocking persistence retains its OS locks after async cancellation. Prefer these existing boundaries over additional public traits or configurable global state.

`LogResponse::chunk()` returns `Result<Option<bytes::Bytes>, bb_api::Error>`. Streaming consumers can borrow chunks as `&[u8]`; callers requiring an owned `Vec<u8>` explicitly call `chunk.to_vec()`. Preserve CLI/JSON contracts, credential records, keyring service/account identifiers, cache fingerprints, and SQLite schemas when extending the application. Credentials use `keyring-core` with an explicit native store for each platform.

Agent instructions are embedded with `include_str!` from the CLI crate's `skills/` directory. Keep that source in Cargo packages. Skill installation is dispatched before Bitbucket configuration/authentication is loaded, but after JSON selection validation; invalid output selections and bare JSON discovery must not mutate local files.

Log search, snippets, byte offsets, and highlights share SQLite FTS5 matching semantics. Snapshot reads keep index text and source bytes consistent during concurrent rewrites. The renderer only consumes prepared spans and clips whole graphemes by terminal width. Search generations include their scope, while terminal step transitions invalidate earlier downloads before requesting a final tail.

Measure changes with the ignored release tests; they use temporary storage and a loopback server:

```sh
cargo test --release -p bb-cli --locked benchmark_ -- --ignored --nocapture
cargo test --release -p bb-cli --locked measure_highlighted_window -- --ignored --nocapture
```

The first command compares a 10,000-item panel merge and a 1 MiB download framed in 1 KiB or 64 KiB chunks. The second measures a highlighted 2,000-line cache window. These are local diagnostic measurements, not timing assertions in CI.

Local Linux release measurements on 2026-10-10:

| Operation | Before maintenance | After maintenance |
| --- | ---: | ---: |
| Merge 10,000 panel items | 108.5 ms | 0.98 ms |
| Ingest 1 MiB in 1 KiB chunks | 4,174 ms | 364 ms |
| Ingest 1 MiB in 64 KiB chunks | 387 ms | 328 ms |

The panel values are medians of three runs; downloads are single runs and depend on storage and server scheduling. During implementation, batching FTS highlighting also reduced the median time for a 2,000-line window from 199.3 ms (one query per line) to 9.2 ms (one cursor per window), over three runs of each implementation. These measurements do not imply equivalent gains for every workload.
