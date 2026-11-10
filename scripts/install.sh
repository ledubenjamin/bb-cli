#!/bin/sh
# Install or update a published bb binary without requiring Rust or root access.
set -eu

fail() {
    printf 'bb install: %s\n' "$*" >&2
    exit 1
}

download() {
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 "$@"
}

main() {
    for bb_command in curl tar mktemp; do
        command -v "$bb_command" >/dev/null 2>&1 || fail "Required command missing: $bb_command"
    done
    case "$(uname -s)/$(uname -m)" in
        Linux/x86_64) bb_target=x86_64-unknown-linux-gnu ;;
        Linux/aarch64|Linux/arm64) bb_target=aarch64-unknown-linux-gnu ;;
        Darwin/x86_64) bb_target=x86_64-apple-darwin ;;
        Darwin/arm64) bb_target=aarch64-apple-darwin ;;
        *) fail "Supported systems: Linux and macOS on x86_64 or ARM64." ;;
    esac
    if command -v sha256sum >/dev/null 2>&1; then
        bb_hash_command=sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        bb_hash_command=shasum
    else
        fail 'Install sha256sum or shasum to verify the download.'
    fi

    bb_releases=https://github.com/ledubenjamin/bb-cli/releases
    bb_tag=${BB_VERSION:-}
    if [ -z "$bb_tag" ]; then
        bb_tag=$(download "$bb_releases/latest/download/VERSION")
    fi
    case "$bb_tag" in v[0-9]*) ;; *) fail 'Expected a version tag such as v0.1.0.' ;; esac
    case "$bb_tag" in *[!a-zA-Z0-9.+-]*) fail 'Invalid version tag.' ;; esac

    bb_install_dir=${BB_INSTALL_DIR:-"$HOME/.local/bin"}
    bb_temporary=$(mktemp -d)
    bb_staged=
    trap 'rm -rf "$bb_temporary"; if [ -n "$bb_staged" ]; then rm -f "$bb_staged"; fi' 0
    trap 'exit 130' INT
    trap 'exit 143' TERM
    bb_name=bb-${bb_tag#v}-$bb_target
    bb_archive=$bb_name.tar.gz
    # Resolve latest once, then fetch both files from the same versioned release.
    download "$bb_releases/download/$bb_tag/$bb_archive" --output "$bb_temporary/$bb_archive"
    download "$bb_releases/download/$bb_tag/$bb_archive.sha256" --output "$bb_temporary/checksum"
    bb_expected=$(awk -v name="$bb_archive" '$2 == name {print $1}' "$bb_temporary/checksum")
    if [ "$bb_hash_command" = sha256sum ]; then
        bb_actual=$(sha256sum "$bb_temporary/$bb_archive")
    else
        bb_actual=$(shasum -a 256 "$bb_temporary/$bb_archive")
    fi
    if [ -z "$bb_expected" ] || [ "$bb_expected" != "${bb_actual%% *}" ]; then
        fail 'SHA-256 verification failed.'
    fi

    tar -xzf "$bb_temporary/$bb_archive" -C "$bb_temporary" "$bb_name/bb"
    bb_binary=$bb_temporary/$bb_name/bb
    if [ ! -f "$bb_binary" ] || [ -L "$bb_binary" ]; then
        fail 'The archive does not contain a regular bb executable.'
    fi
    chmod 755 "$bb_binary"
    bb_actual_version=$("$bb_binary" --version) || fail 'The binary cannot run on this system. Check the README runtime requirements.'
    [ "$bb_actual_version" = "bb ${bb_tag#v}" ] || fail 'The downloaded binary has the wrong version.'

    mkdir -p "$bb_install_dir"
    [ ! -d "$bb_install_dir/bb" ] || fail 'The installation path is a directory.'
    # Rename within the destination filesystem so a failed update keeps the old binary.
    bb_staged=$(mktemp "$bb_install_dir/.bb.XXXXXX")
    cp "$bb_binary" "$bb_staged"
    chmod 755 "$bb_staged"
    mv -f "$bb_staged" "$bb_install_dir/bb"
    bb_staged=
    printf 'Installed %s to %s/bb\n' "$bb_actual_version" "$bb_install_dir"
    case ":$PATH:" in
        *":$bb_install_dir:"*) ;;
        *) printf 'Add %s to your PATH, then run bb --version.\n' "$bb_install_dir" ;;
    esac
}

main "$@"
