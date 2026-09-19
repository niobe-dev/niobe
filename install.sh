#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

# Installs the niobe binary from a GitHub Release.
#
#   curl -fsSL https://github.com/niobe-dev/niobe/releases/latest/download/install.sh | sh
#
# Settings, all optional, read from the environment:
#   NIOBE_VERSION       the release to install, e.g. v0.1.0 (default: the latest)
#   NIOBE_INSTALL_DIR   where the binary goes (default: ~/.local/bin)
#   NIOBE_DOWNLOAD_URL  where the release files are fetched from, instead of
#                       GitHub; any URL curl can read, file:// included
#
# The archive is checked against the SHA-256 published beside it before
# anything is installed, and nothing outside the install directory is touched:
# no shell profile is edited, no sudo is asked for.
#
# POSIX sh only, and the whole script is one function called on its last line,
# so a download cut off half way through runs nothing.

set -eu

REPO="niobe-dev/niobe"

say() {
    printf 'niobe-install: %s\n' "$*" >&2
}

fail() {
    say "error: $*"
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || fail "needs '$1', which is not on PATH"
}

# The Rust target triple this machine runs, as the release archives are named.
target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Darwin)
            # A shell under Rosetta reports x86_64 on Apple silicon; the
            # native binary is the one to install there.
            if [ "$arch" = "x86_64" ] &&
                [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = "1" ]; then
                arch=arm64
            fi
            os=apple-darwin
            ;;
        Linux) os=unknown-linux-musl ;;
        *) fail "no release is built for $os; build from source with cargo" ;;
    esac
    case "$arch" in
        arm64 | aarch64) arch=aarch64 ;;
        x86_64 | amd64) arch=x86_64 ;;
        *) fail "no release is built for $arch; build from source with cargo" ;;
    esac
    printf '%s-%s\n' "$arch" "$os"
}

download() {
    if command -v curl >/dev/null 2>&1; then
        curl --proto '=https,file' --tlsv1.2 -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --https-only "$1" -O "$2"
    else
        fail "needs curl or wget to download the release"
    fi
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    else
        fail "needs sha256sum or shasum to verify the download"
    fi
}

main() {
    need uname
    need tar
    need mktemp

    version=${NIOBE_VERSION:-latest}
    install_dir=${NIOBE_INSTALL_DIR:-"$HOME/.local/bin"}
    case "$version" in
        latest) base="https://github.com/$REPO/releases/latest/download" ;;
        v*) base="https://github.com/$REPO/releases/download/$version" ;;
        *) base="https://github.com/$REPO/releases/download/v$version" ;;
    esac
    base=${NIOBE_DOWNLOAD_URL:-$base}

    triple=$(target)
    archive="niobe-$triple.tar.gz"

    scratch=$(mktemp -d)
    trap 'rm -rf "$scratch"' EXIT INT TERM

    say "downloading $archive ($version)"
    download "$base/$archive" "$scratch/$archive" ||
        fail "could not download $base/$archive"
    download "$base/$archive.sha256" "$scratch/$archive.sha256" ||
        fail "could not download $base/$archive.sha256"

    expected=$(cut -d ' ' -f 1 <"$scratch/$archive.sha256")
    actual=$(sha256 "$scratch/$archive")
    if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
        fail "$archive does not match its published SHA-256 (expected $expected, got $actual); nothing was installed"
    fi

    tar -xzf "$scratch/$archive" -C "$scratch"
    [ -f "$scratch/niobe" ] || fail "$archive holds no niobe binary"

    mkdir -p "$install_dir"
    # Copied beside the target and renamed over it, so a niobe that is running
    # keeps its file and a failed copy leaves the old one in place.
    cp "$scratch/niobe" "$install_dir/.niobe.new"
    chmod 755 "$install_dir/.niobe.new"
    mv -f "$install_dir/.niobe.new" "$install_dir/niobe"

    say "installed $("$install_dir/niobe" --version) to $install_dir/niobe"
    case ":$PATH:" in
        *":$install_dir:"*) ;;
        *)
            say "$install_dir is not on your PATH; add it to your shell profile:"
            say "  export PATH=\"$install_dir:\$PATH\""
            ;;
    esac
    command -v claude >/dev/null 2>&1 ||
        say "niobe drives the official 'claude' CLI, which is not on PATH yet"
}

main "$@"
