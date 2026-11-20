#!/bin/sh
# Builds cpd and cpctl for one Rust target and packs the archive the
# installer expects: clanker-proxy_<os>_<arch>.tar.gz holding cpd, cpctl,
# LICENSE and README.md, flat. The binaries are also left in
# $DIST_DIR/<os>-<arch>/ for the container image.
set -eu

if [ "$#" -ne 3 ]; then
    printf 'usage: %s <rust-target> <linux|darwin> <amd64|arm64>\n' "$0" >&2
    exit 1
fi
target=$1
os=$2
arch=$3
dist=${DIST_DIR:-build/dist}

cargo build --release --locked --target "$target" --package cpd --package cpctl

bin=$dist/$os-$arch
mkdir -p "$bin"
for binary in cpd cpctl; do
    cp "target/$target/release/$binary" "$bin/$binary"
    chmod 755 "$bin/$binary"
done

stage=$(mktemp -d "${TMPDIR:-/tmp}/clanker-proxy-package.XXXXXXXX")
trap 'rm -rf "$stage"' 0
trap 'exit 1' HUP INT TERM
cp "$bin/cpd" "$bin/cpctl" LICENSE README.md "$stage/"
tar -czf "$dist/clanker-proxy_${os}_${arch}.tar.gz" -C "$stage" cpd cpctl LICENSE README.md
printf '%s\n' "$dist/clanker-proxy_${os}_${arch}.tar.gz"
