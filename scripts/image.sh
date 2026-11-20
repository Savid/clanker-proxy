#!/bin/sh
# Builds clanker-proxy:local for this machine's architecture: static musl
# binaries, then the release Dockerfile. Needs Docker, and musl-gcc
# (musl-tools) for the C parts of the build.
set -eu

case $(uname -m) in
    x86_64|amd64) arch=amd64 target=x86_64-unknown-linux-musl ;;
    aarch64|arm64) arch=arm64 target=aarch64-unknown-linux-musl ;;
    *) printf 'image: unsupported architecture %s\n' "$(uname -m)" >&2; exit 1 ;;
esac
rustup target add "$target"
sh scripts/package.sh "$target" linux "$arch" >/dev/null
docker build --platform "linux/$arch" -t clanker-proxy:local .
