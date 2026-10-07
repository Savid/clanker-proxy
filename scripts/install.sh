#!/bin/sh
set -eu

fail() {
    printf 'install: %s\n' "$*" >&2
    exit 1
}

download() {
    curl --disable --fail --silent --show-error --location \
        --proto '=https' --proto-redir '=https' \
        --connect-timeout 10 --max-time 120 --retry 2 \
        --max-filesize 104857600 "$@"
}

cleanup() {
    if [ "$rollback" = 1 ]; then
        for binary in cpd cpctl; do
            # A source still in staging means its rename never happened.
            if [ -f "$stage/$binary" ]; then continue; fi
            if [ -f "$stage/$binary.old" ]; then
                mv -f "$stage/$binary.old" "$install_dir/$binary" || {
                    printf 'install: restore failed; backups remain in %s\n' "$stage" >&2
                    return 1
                }
            else
                rm -f "$install_dir/$binary"
            fi
        done
    fi
    rm -rf "$tmp"
    if [ -n "$stage" ]; then rm -rf "$stage"; fi
    if [ -n "$lock" ]; then rmdir "$lock"; fi
}

main() {
    for tool in curl tar awk mktemp; do
        command -v "$tool" >/dev/null 2>&1 || fail "$tool is required"
    done
    if command -v sha256sum >/dev/null 2>&1; then
        checksum=sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        checksum=shasum
    else
        fail 'sha256sum or shasum is required'
    fi

    case $(uname -s) in
        Linux) os=linux ;;
        Darwin) os=darwin ;;
        *) fail 'supported systems: Linux and macOS' ;;
    esac
    case $(uname -m) in
        x86_64|amd64) arch=amd64 ;;
        aarch64|arm64) arch=arm64 ;;
        *) fail 'supported architectures: amd64 and arm64' ;;
    esac

    repo=https://github.com/Savid/clanker-proxy
    version=${CP_VERSION:-}
    if [ -z "$version" ]; then
        latest=$(download --head --output /dev/null --write-out '%{url_effective}' "$repo/releases/latest") ||
            fail 'cannot find the latest release; check https://github.com/Savid/clanker-proxy/releases'
        case $latest in
            https://github.com/Savid/clanker-proxy/releases/tag/*|https://github.com/savid/clanker-proxy/releases/tag/*)
                version=${latest##*/} ;;
            *) fail 'GitHub did not return a release tag' ;;
        esac
    fi
    printf '%s\n' "$version" | awk '/^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/ { valid++ } END { exit !(NR == 1 && valid == 1) }' ||
        fail 'CP_VERSION must be a stable release tag, such as v1.2.3'

    install_dir=${CP_INSTALL_DIR:-${HOME:?HOME is required}/.local/bin}
    case $install_dir in
        /*) ;;
        *) fail 'CP_INSTALL_DIR must be an absolute path' ;;
    esac
    for binary in cpd cpctl; do
        [ ! -L "$install_dir/$binary" ] || fail "$install_dir/$binary is a symlink; choose another CP_INSTALL_DIR"
        [ ! -d "$install_dir/$binary" ] || fail "$install_dir/$binary is a directory"
        if [ -e "$install_dir/$binary" ] && [ ! -f "$install_dir/$binary" ]; then
            fail "$install_dir/$binary is not a regular file"
        fi
    done

    tmp=$(mktemp -d "${TMPDIR:-/tmp}/clanker-proxy.XXXXXXXX")
    stage=
    lock=
    rollback=0
    trap cleanup 0
    trap 'exit 1' HUP INT TERM

    mkdir -p "$install_dir"
    if mkdir "$install_dir/.clanker-proxy-update.lock" 2>/dev/null; then
        lock=$install_dir/.clanker-proxy-update.lock
    else
        fail "cannot lock $install_dir; check permissions or another update in progress (remove .clanker-proxy-update.lock only if no installer or updater is running)"
    fi

    asset=clanker-proxy_${os}_${arch}.tar.gz
    base=$repo/releases/download/$version
    printf 'Installing clanker-proxy %s (%s/%s)\n' "$version" "$os" "$arch"
    download --output "$tmp/$asset" "$base/$asset" || fail "cannot download $asset for $version"
    download --output "$tmp/checksums.txt" "$base/checksums.txt" || fail 'cannot download checksums.txt'
    expected=$(awk -v name="$asset" '$2 == name { count++; hash=$1 } END { if (count != 1) exit 1; print hash }' "$tmp/checksums.txt") ||
        fail 'release must contain exactly one checksum for the archive'
    [ "${#expected}" -eq 64 ] || fail 'invalid SHA-256 checksum'
    case $expected in *[!0-9a-f]*) fail 'invalid SHA-256 checksum' ;; esac
    if [ "$checksum" = sha256sum ]; then
        actual=$(sha256sum "$tmp/$asset")
    else
        actual=$(shasum -a 256 "$tmp/$asset")
    fi
    [ "${actual%% *}" = "$expected" ] || fail 'SHA-256 checksum mismatch; nothing was installed'

    tar -tzf "$tmp/$asset" > "$tmp/contents" || fail 'invalid release archive'
    # tar selects descendants too; an extra cpctl/child would append to cpctl.
    awk '$0 != "cpd" && $0 != "cpctl" && $0 != "LICENSE" && $0 != "README.md" { exit 1 }' "$tmp/contents" ||
        fail 'archive contains an unexpected member'
    for binary in cpd cpctl; do
        awk -v name="$binary" '$0 == name { count++ } END { exit count != 1 }' "$tmp/contents" ||
            fail "archive must contain exactly one $binary"
    done
    stage=$(mktemp -d "$install_dir/.clanker-proxy.XXXXXXXX") || fail "cannot write to $install_dir"
    # Extract to stdout so archive paths and links cannot escape the staging directory.
    for binary in cpd cpctl; do
        tar -xzOf "$tmp/$asset" "$binary" > "$stage/$binary" || fail "cannot extract $binary"
        [ -s "$stage/$binary" ] || fail "archive contains an empty or non-regular $binary"
        chmod 755 "$stage/$binary"
    done
    for binary in cpd cpctl; do
        if [ -f "$install_dir/$binary" ]; then
            ln "$install_dir/$binary" "$stage/$binary.old"
        fi
    done
    # Same-filesystem renames leave an already running daemon's executable intact.
    rollback=1
    for binary in cpd cpctl; do
        mv -f "$stage/$binary" "$install_dir/$binary"
    done
    rollback=0
    printf 'Installed cpd and cpctl in %s\n' "$install_dir"
    case :${PATH:-}: in
        *:"$install_dir":*) ;;
        *) printf 'Add %s to your PATH.\n' "$install_dir" ;;
    esac
    printf 'Restart any running cpd to use %s.\nnext: cpctl help\n' "$version"
}

# A truncated download must not execute a partial installation.
main "$@"
