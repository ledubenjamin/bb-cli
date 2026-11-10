# Installation and updates


Download a ready-to-run binary from the [latest release](https://github.com/ledubenjamin/bb-cli/releases/latest). **Rust is not required.** Install Git for clone, checkout, rebase, and remote detection. The release matrix covers Linux x86_64/ARM64 and Windows x86_64. Ubuntu 24.04 and 26.04 are tested explicitly. macOS has no prebuilt release; build from source and validate it in your environment.

**Development branch:** Windows binaries and `bb skills` will ship in the next release. Published v0.1.2 archives contain Linux binaries only.

## Linux x86_64 and ARM64

The installer detects x86_64 or ARM64, verifies the archive's SHA-256 checksum, and installs `bb` into `~/.local/bin` without `sudo`:

```sh
curl -fsSL https://github.com/ledubenjamin/bb-cli/releases/latest/download/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
bb --version
```

Add the `export PATH` line to your shell configuration (`~/.bashrc` or `~/.zshrc`) once. Set `BB_INSTALL_DIR` when invoking the installer to choose another directory. Linux binaries require glibc 2.35 or newer (Ubuntu 22.04 baseline) and `libdbus-1`; on Ubuntu/Debian, install the runtime with `sudo apt-get install libdbus-1-3`. Saving credentials also requires a running Secret Service session.

## Windows x86_64

Use 64-bit PowerShell 5.1 or PowerShell 7. Download the installer, inspect it, then run it:

```powershell
Invoke-WebRequest https://github.com/ledubenjamin/bb-cli/releases/latest/download/install.ps1 -OutFile install-bb.ps1
Get-Content .\install-bb.ps1
.\install-bb.ps1
bb --version
```

The installer verifies SHA-256 and installs `bb.exe` into `%LOCALAPPDATA%\bb\bin`. It updates your user PATH; open a new terminal if needed. Use `-InstallDir C:\Tools\bb` for another destination or `-NoPathUpdate` to manage PATH yourself. If your execution policy blocks downloaded scripts, inspect the script and follow your organization's policy; the manual ZIP installation also works.

Stop running `bb` processes before updating on Windows. A locked executable or failed verification leaves the previous binary intact. The native keyring uses Windows Credential Manager. Windows CI runs on Windows Server 2025; Windows 11 is a desktop target but is not covered by a hosted desktop test.

## Update

**Run the same installation command again** to replace `bb` with the latest stable version. Your profiles, credentials, configuration, and log cache are preserved. If the download, checksum, or binary verification fails, the existing installation remains in place. If you originally installed with Cargo, remove that copy with `cargo uninstall bb-cli` before switching to the release installer so your `PATH` selects the new binary.

Check your installed version with `bb --version` and compare it with the [latest release](https://github.com/ledubenjamin/bb-cli/releases/latest). On GitHub, choose **Watch → Custom → Releases** to receive notifications. Update when a new release contains a fix or feature you need; prioritize security fixes and read the release notes for breaking changes. `bb` does not check for updates or replace itself during normal commands.

To install a particular version, use its installer and pass its tag:

```sh
curl -fsSL https://github.com/ledubenjamin/bb-cli/releases/download/v0.1.2/install.sh | BB_VERSION=v0.1.2 sh
```

Previews such as `v0.2.0-rc.1` require an explicit version; the default installer follows stable releases.

## Manual download

The release workflow produces the following archives, individual `.sha256` files, and `SHA256SUMS`:

| Platform | Archive |
| --- | --- |
| Linux x86_64 | `bb-VERSION-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `bb-VERSION-aarch64-unknown-linux-gnu.tar.gz` |
| Windows x86_64 | `bb-VERSION-x86_64-pc-windows-msvc.zip` |

Replace `VERSION` with the version number without `v`. Extract the archive and put `bb` in a directory on your `PATH`. Archives also contain the JSON schema, shell completions, the official `skills/bb-cli/SKILL.md`, documentation, and dependency notices. For a private repository, download from the authenticated Releases page or use `gh release download --repo ledubenjamin/bb-cli` after `gh auth login`.

## Build from source

Rust **1.99.0 or newer** is required to build. The repository pins Rust 1.99.0 for reproducible development.

```sh
cargo build --release --locked
./target/release/bb --help  # Windows: .\target\release\bb.exe --help
# Optional installation into Cargo's bin directory:
cargo install --path crates/bb-cli --locked
```

On Linux, building the system keyring backend requires a C/C++ toolchain, CMake, `pkg-config`, and the D-Bus development package (`libdbus-1-dev` on Ubuntu/Debian). In CI, provide `BB_TOKEN` instead of a keyring.


On Windows, install the Rust MSVC toolchain and Visual Studio Build Tools with **Desktop development with C++** (including the Windows SDK and CMake). Git and Python 3 are needed for the development tests. No POSIX shell is required to run `bb.exe`.

## Remove bb

Delete the executable from its installation directory, or use `cargo uninstall bb-cli` for a Cargo installation. Installed agent instructions are separate: remove them first with `bb skills remove --agent codex` (or `claude`, with the same scope used for installation). Use `bb auth logout` to remove saved local credentials before uninstalling; revoking a token in Bitbucket is a separate action. Configuration and cached logs remain unless you explicitly remove them.
