#!/usr/bin/env bash
# Stage bin/roboherd, which every manifest entrypoint runs.
#
# A released binary matching this manifest's version is preferred and building from source is the
# fallback, so an install needs a Rust toolchain only where no published asset matches.
#
# herdr runs build commands as bare argv with no shell, so this cannot be a list of manifest steps.
set -euo pipefail

readonly REPO="andschneider/roboherd"
readonly BIN="roboherd"

cd "$(dirname "$0")/.."

work=""
cleanup() {
    if [ -n "$work" ]; then
        rm -rf "$work"
    fi
}
trap cleanup EXIT

# The manifest is the release tag's source of truth, so the tag cannot drift from what herdr read.
manifest_version() {
    grep -m1 '^version' herdr-plugin.toml | cut -d'"' -f2
}

# The release target this machine can run, or nothing when no asset is published for it.
target_triple() {
    case "$(uname -s) $(uname -m)" in
    "Darwin arm64") echo "aarch64-apple-darwin" ;;
    "Darwin x86_64") echo "x86_64-apple-darwin" ;;
    "Linux x86_64") echo "x86_64-unknown-linux-musl" ;;
    "Linux aarch64" | "Linux arm64") echo "aarch64-unknown-linux-musl" ;;
    *) return 1 ;;
    esac
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# Put a binary at bin/roboherd.
#
# The reporter runs from this path, and overwriting a mapped Mach-O in place makes macOS SIGKILL
# every later exec. Renaming over it is atomic and gives the replacement its own inode, so a running
# reporter keeps the file it mapped and no one ever observes a half-copied binary. The staging name
# is a sibling to keep the rename inside one filesystem.
# Every step is checked rather than left to `set -e`, which bash suspends for the whole dynamic
# extent of a function called as an `if` condition, as `download` is below.
install_binary() {
    local staged="bin/.$BIN.new"
    mkdir -p bin || return 1
    # A staging file left by an interrupted run would otherwise be what the rename promotes.
    rm -f "$staged" || return 1
    cp "$1" "$staged" || return 1
    chmod +x "$staged" || return 1
    mv -f "$staged" "bin/$BIN" || return 1
}

# Fetch the published binary for this platform. Any miss returns non-zero and the caller compiles.
download() {
    local version target asset url want got
    version="$(manifest_version)" || return 1
    target="$(target_triple)" || return 1
    [ -n "$version" ] || return 1

    command -v curl >/dev/null 2>&1 || return 1
    command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 || return 1

    asset="$BIN-$target.tar.gz"
    url="https://github.com/$REPO/releases/download/v$version/$asset"
    work="$(mktemp -d)" || return 1

    # Release assets are eventually consistent, so a fresh publish can 404 for a few minutes and a
    # 404 has to be retried like any other error. Kept short, since a platform with no asset at all
    # pays this wait before falling through to a compile that takes minutes anyway.
    local retry=(--retry 3 --retry-delay 2 --retry-all-errors --retry-connrefused)
    # A miss here is the ordinary path on an unreleased platform, and the caller reports it, so
    # curl's own errors would only look like a failed install in herdr's build log.
    curl -fsSL "${retry[@]}" -o "$work/$asset" "$url" 2>/dev/null || return 1
    curl -fsSL "${retry[@]}" -o "$work/$asset.sha256" "$url.sha256" 2>/dev/null || return 1

    want="$(cut -d' ' -f1 <"$work/$asset.sha256")" || return 1
    got="$(sha256 "$work/$asset")" || return 1
    # The length check keeps two blank results from comparing equal and passing an unverified
    # binary through, since neither side aborts the function on its own.
    if [ ${#want} -ne 64 ] || [ "$want" != "$got" ]; then
        echo "roboherd: checksum mismatch for $asset" >&2
        return 1
    fi

    tar -xzf "$work/$asset" -C "$work" || return 1
    [ -f "$work/$BIN" ] || return 1

    # Proves the asset runs here, which a checksum cannot. Catches a mislabeled architecture and a
    # C library this machine cannot satisfy.
    "$work/$BIN" --version >/dev/null 2>&1 || return 1

    install_binary "$work/$BIN" || return 1
}

if download; then
    echo "roboherd: staged bin/$BIN from release v$(manifest_version)"
else
    echo "roboherd: no published binary was usable here, building from source"
    cargo build --release --locked
    install_binary "target/release/$BIN"
fi
