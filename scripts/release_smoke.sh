#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
    printf 'usage: %s <native-release.tar.gz> <expected-version>\n' "$0" >&2
    exit 1
fi

archive=$1
expected=$2
work=$(mktemp -d "${TMPDIR:-/tmp}/clanker-proxy-smoke.XXXXXXXX")
trap 'rm -rf "$work"' 0
trap 'exit 1' HUP INT TERM

for binary in cpd cpctl; do
    tar -xzOf "$archive" "$binary" > "$work/$binary"
    chmod 755 "$work/$binary"
    actual=$("$work/$binary" -version)
    if [ "$actual" != "$expected" ]; then
        printf '%s: version %s does not match release %s\n' "$binary" "$actual" "$expected" >&2
        exit 1
    fi
    printf '%s %s: release smoke test passed\n' "$binary" "$actual"
done
