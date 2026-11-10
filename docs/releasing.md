# Releases and public distribution


1. Set `workspace.package.version` and the local `bb-api` dependency version in `Cargo.toml`, refresh `Cargo.lock` with `cargo check --workspace`, and commit the release changes after CI passes.
2. Tag that commit with the matching version and push the tag:

   ```sh
   git tag -a v0.2.0 -m "Release v0.2.0"
   git push origin v0.2.0
   ```

The [Release workflow](../.github/workflows/release.yml) checks that the tag matches Cargo's version, tests and builds Linux x86_64/ARM64 and Windows x86_64, packages the binaries, and tests installation and updates from each archive. Linux archives are built on Ubuntu 22.04 and exercised unchanged on Ubuntu 24.04 and 26.04 for both architectures. Windows checks cover both PowerShell 5.1 and 7. Once all builds and archive checks succeed, it uploads the archives, checksums, installers, and generated release notes, then **publishes the release automatically**. A newer stable version becomes **Latest**; finishing an older build later does not replace it. Tags containing a prerelease suffix are marked as prereleases. The repository's `GITHUB_TOKEN` supplies the required release permission.

You can also choose **Actions → Release → Run workflow** and enter an existing version tag. A failed upload leaves a draft that a rerun can finish; an already published release keeps its assets unchanged. Publish a new version tag to distribute another build. Ordinary branch pushes run CI; they do not publish a release.

## Before making the repository public

- Run the [contributor checks](development.md) and inspect native CI results. Keep the distinction between a configured job and a successful run.
- Scan both the complete Git history and the current tracked files for credentials; handle any real findings before changing visibility. Avoid putting scan reports containing secrets in the repository.
- Review LICENSE, NOTICE, bundled dependency notices, public examples, screenshots, and links. Use fictional Bitbucket workspaces in examples.
- Enable GitHub private vulnerability reporting and secret scanning/push protection when available for the public repository. Verify the [security report link](https://github.com/ledubenjamin/bb-cli/security/advisories/new).
- Change repository visibility in GitHub only when ready. Visibility changes and tag pushes are separate actions: a matching `v*` tag starts automatic release publication.
- After publishing the new version, remove the development-branch notice from the README and installation guide, and verify downloads without GitHub authentication.

Update CHANGELOG.md and the versioned dependencies together. Published assets are immutable: use a new version if a release needs a correction. Never reuse `v0.1.2` for the Windows or skills additions.
