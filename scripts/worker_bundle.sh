#!/bin/sh
# Packs the built Worker (make worker-build) into
# $DIST_DIR/clanker-proxy_worker.tar.gz: everything wrangler needs to deploy
# it with Node alone, with no Rust toolchain and no build step.
set -eu

dist=${DIST_DIR:-build/dist}
src=crates/cpd-worker/build
[ -f "$src/index_bg.wasm" ] || { echo "worker bundle: run make worker-build first" >&2; exit 1; }

stage=$(mktemp -d "${TMPDIR:-/tmp}/clanker-proxy-worker.XXXXXXXX")
trap 'rm -rf "$stage"' 0
trap 'exit 1' HUP INT TERM

mkdir -p "$stage/build/worker" "$dist"
cp "$src/index.js" "$src/index_bg.wasm" "$stage/build/"
cp "$src/worker/shim.mjs" "$stage/build/worker/"
cp LICENSE "$stage/"

# The repository's config, minus the build step: the bundle is prebuilt.
# shellcheck disable=SC2016 # JavaScript, not shell: nothing to expand.
node -e '
const fs = require("fs");
const text = fs.readFileSync("wrangler.jsonc", "utf8").split("\n").filter((l) => !/^\s*\/\//.test(l)).join("\n");
const c = JSON.parse(text);
delete c.build;
delete c["$schema"];
c.main = "build/worker/shim.mjs";
fs.writeFileSync(process.argv[1], JSON.stringify(c, null, 2) + "\n");
' "$stage/wrangler.json"

# The wrangler release this repository tests with, locked.
wrangler=$(node -p 'require("./package.json").devDependencies.wrangler')
printf '{\n  "private": true,\n  "devDependencies": { "wrangler": "%s" }\n}\n' "$wrangler" > "$stage/package.json"
(cd "$stage" && npm install --package-lock-only --no-audit --no-fund >/dev/null)

tar -czf "$dist/clanker-proxy_worker.tar.gz" -C "$stage" .
printf '%s\n' "$dist/clanker-proxy_worker.tar.gz"
