#!/bin/sh
# Tests upgrading to this build from a release: CP_PREVIOUS (a tag), else
# the newest release this commit is built on. Daemons of that release make
# real data, one is upgraded in place, and it keeps working with peers
# still on that release; then the conformance suite runs between this
# build and that release, unless --no-conformance (for releases too old to
# pass it). Downloads the release; needs cargo, cargo-nextest and python3.
#
#   sh scripts/upgrade_test.sh [--no-conformance]
set -eu
conformance=yes
[ "${1:-}" != --no-conformance ] || conformance=

repo=https://github.com/Savid/clanker-proxy
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/clanker-proxy-upgrade.XXXXXXXX")
pids=
cleanup() {
    for p in $pids; do kill "$p" 2>/dev/null || true; done
    rm -rf "$work"
}
trap cleanup 0
trap 'exit 1' HUP INT TERM

fail() {
    printf 'upgrade_test: %s\n' "$*" >&2
    for log in "$work"/*.log; do
        [ -f "$log" ] || continue
        printf '  == %s\n' "${log##*/}" >&2
        tail -n 15 "$log" | sed 's/^/  | /' >&2
    done
    exit 1
}

case $(uname -s)/$(uname -m) in
    Linux/x86_64) asset=linux_amd64 ;;
    Linux/aarch64 | Linux/arm64) asset=linux_arm64 ;;
    Darwin/x86_64) asset=darwin_amd64 ;;
    Darwin/arm64) asset=darwin_arm64 ;;
    *) fail "no release build for $(uname -s)/$(uname -m)" ;;
esac
# A branch behind the latest release upgrades from the one it is built
# on: data from a newer release is refused, rightly. A release commit
# upgrades from the release before its own.
here=$(git -C "$root" tag --points-at HEAD --list 'v[0-9]*' 2>/dev/null | sed 's/^/--exclude=/')
# shellcheck disable=SC2086 # one option per tag; tags have no spaces
previous=${CP_PREVIOUS:-$(git -C "$root" describe --tags --abbrev=0 --match 'v[0-9]*' $here 2>/dev/null || true)}
if [ -z "$previous" ]; then
    previous=$(curl -fsSIL --retry 3 -o /dev/null -w '%{url_effective}' "$repo/releases/latest" | sed 's|.*/||')
fi
case $previous in
    v[0-9]*) ;;
    *) fail "no release to upgrade from" ;;
esac
base=$repo/releases/download/$previous
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1
}
archive=clanker-proxy_$asset.tar.gz
curl -fsSL --retry 3 -o "$work/$archive" "$base/$archive" || fail "download $base/$archive"
curl -fsSL --retry 3 -o "$work/checksums.txt" "$base/checksums.txt" || fail "download $base/checksums.txt"
want=$(sed -n "s/^\([0-9a-f]\{64\}\)  $archive\$/\1/p" "$work/checksums.txt")
if [ -z "$want" ] || [ "$(sha256 "$work/$archive")" != "$want" ]; then
    fail "$archive does not match checksums.txt"
fi
mkdir "$work/old"
tar -xzf "$work/$archive" -C "$work/old"
old=$work/old

# Where cargo put them, wherever its configuration says.
built=$(cd "$root" && cargo build --quiet --locked -p cpd -p cpctl --message-format=json-render-diagnostics |
    python3 -I -c 'import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable"):
        print(m["executable"])') || fail "build"
new=$(printf '%s\n' "$built" | sed -n 's|/cpd$||p')
if [ ! -x "$new/cpd" ] || [ ! -x "$new/cpctl" ]; then fail "build: no cpd and cpctl side by side"; fi

# start <dir> <port> <cpd> [<name>]: a daemon on loopback, in $work/<dir>,
# named after it unless named; $started is its process.
start() {
    if curl -fsS "http://127.0.0.1:$2/api/v1/health" >/dev/null 2>&1; then
        fail "port $2 is already in use"
    fi
    CP_DIR=$work/$1 "$3" --name "${4:-$1}" --url "http://127.0.0.1:$2" --listen "127.0.0.1:$2" >>"$work/$1.log" 2>&1 &
    started=$!
    pids="$pids $started"
    i=0
    until curl -fsS "http://127.0.0.1:$2/api/v1/health" >/dev/null 2>&1; do
        i=$((i + 1))
        [ "$i" -lt 50 ] || fail "$1 did not start"
        sleep 0.2
    done
    kill -0 "$started" 2>/dev/null || fail "$1 exited"
}

# stop <process>
stop() {
    kill "$1"
    wait "$1" 2>/dev/null || true
}

# query <database> <sql>: rows as text, one per line.
query() {
    python3 -I -c 'import sqlite3, sys
for row in sqlite3.connect(sys.argv[1]).execute(sys.argv[2]):
    print("|".join("" if v is None else str(v) for v in row))' "$@"
}
stored() { query "$1" "SELECT value FROM meta WHERE key = 'schema_version'"; }
schema() { query "$1" "SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name"; }

# ctl <name> <port> <cpctl> <args>...: cpctl as that daemon's owner.
ctl() {
    d=$1 port=$2 cli=$3
    shift 3
    HOME=$work XDG_CONFIG_HOME=$work/config CP_NO_UPDATE_CHECK=1 CP_URL="http://127.0.0.1:$port" \
        CP_TOKEN=$(cat "$work/$d/owner.token") "$cli" "$@"
}

# eventually <what> <command>...: retries for 20 seconds.
eventually() {
    what=$1
    shift
    i=0
    until "$@" >/dev/null 2>&1; do
        i=$((i + 1))
        [ "$i" -lt 40 ] || fail "$what"
        sleep 0.5
    done
}

# holds <text> <command>...: the command's output contains the text.
holds() {
    text=$1
    shift
    "$@" 2>&1 | grep -qF -- "$text"
}

# befriend <requester> <port> <cpctl> <approver> <port> <cpctl>
befriend() {
    ctl "$1" "$2" "$3" peer add "$4" "http://127.0.0.1:$5" -m "upgrade test" >/dev/null || fail "$1 could not ask $4"
    id=$(ctl "$4" "$5" "$6" requests --json | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
    [ -n "$id" ] || fail "$4 got no request from $1"
    ctl "$4" "$5" "$6" approve "$id" >/dev/null || fail "$4 could not approve $1"
    eventually "$1 and $4 did not become peers" holds '"status":"active"' ctl "$1" "$2" "$3" peer show "$4" --json
}

# Data made by the previous release, in every table a migration may
# change: a peer with an alias, a thread both wrote in, a former peer, a
# request waiting, an agent token, and a webhook with deliveries.
start alice 18561 "$old/cpd"
alice_pid=$started
start bob 18562 "$old/cpd"
ctl alice 18561 "$old/cpctl" webhook add hook http://127.0.0.1:9/hook >/dev/null || fail "webhook add"
befriend alice 18561 "$old/cpctl" bob 18562 "$old/cpctl"
ctl alice 18561 "$old/cpctl" peer alias bob robert >/dev/null || fail "peer alias"
ref=$(ctl alice 18561 "$old/cpctl" send bob "Upgrade test" -m "Before the upgrade" --json |
    sed -n 's/^{"id":"\([^"]*\)".*/\1/p')
[ -n "$ref" ] || fail "alice could not send"
eventually "bob did not get the thread" holds "$ref" ctl bob 18562 "$old/cpctl" ls --json
ctl bob 18562 "$old/cpctl" reply "$ref" -m "Seen before" >/dev/null || fail "bob could not reply"
eventually "alice did not get bob's reply" holds "Seen before" ctl alice 18561 "$old/cpctl" show "$ref" --all --json
ctl alice 18561 "$old/cpctl" token add agent -o "$work/agent.token" >/dev/null || fail "token add"
start eve 18567 "$old/cpd"
eve_pid=$started
befriend eve 18567 "$old/cpctl" alice 18561 "$old/cpctl"
ctl eve 18567 "$old/cpctl" send alice "Before eve left" -m "From a former peer" >/dev/null || fail "eve could not send"
eventually "alice did not get eve's thread" holds "Before eve left" ctl alice 18561 "$old/cpctl" ls --json
ctl alice 18561 "$old/cpctl" peer rm eve >/dev/null || fail "peer rm"
stop "$eve_pid"
start dave 18564 "$old/cpd"
ctl dave 18564 "$old/cpctl" peer add alice http://127.0.0.1:18561 -m "upgrade test" >/dev/null || fail "dave could not ask alice"

# Alice upgrades in place. A migration of an empty table cannot fail on
# its rows, so every table has some, but the approval that is in flight
# for seconds.
stop "$alice_pid"
for table in $(query "$work/alice/cp.db" "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"); do
    [ "$table" != peering_op ] || continue
    [ "$(query "$work/alice/cp.db" "SELECT EXISTS (SELECT 1 FROM \"$table\")")" = 1 ] ||
        fail "$previous left $table empty: make data for it above"
done
before=$(stored "$work/alice/cp.db")
start alice 18561 "$new/cpd"
alice_pid=$started
A() { ctl alice 18561 "$new/cpctl" "$@"; }
holds alice A me || fail "upgraded: lost the owner's name"
holds '"status":"active"' A peer show bob --json || fail "upgraded: lost the peer"
holds robert A peer show bob --json || fail "upgraded: lost the peer's alias"
holds "Before eve left" A ls --json || fail "upgraded: lost the former peer's thread"
holds "Before the upgrade" A show "$ref" --all --json || fail "upgraded: lost alice's message"
holds "Seen before" A show "$ref" --all --json || fail "upgraded: lost bob's message"
holds '"name":"agent"' A token ls --json || fail "upgraded: lost the agent token"
holds '"name":"hook"' A webhook ls --json || fail "upgraded: lost the webhook"
holds "$ref" env CP_TOKEN="$(cat "$work/agent.token")" HOME="$work" XDG_CONFIG_HOME="$work/config" \
    CP_NO_UPDATE_CHECK=1 CP_URL=http://127.0.0.1:18561 "$new/cpctl" ls --json || fail "upgraded: the agent token stopped working"

# Upgraded data has the schema a fresh daemon of this build makes: a shipped
# migration or STATEMENTS edited since that release would show here.
start fresh 18565 "$new/cpd"
if [ "$(schema "$work/alice/cp.db")" != "$(schema "$work/fresh/cp.db")" ]; then
    schema "$work/alice/cp.db" >"$work/upgraded.schema"
    schema "$work/fresh/cp.db" >"$work/fresh.schema"
    diff "$work/upgraded.schema" "$work/fresh.schema" | head -n 20 >&2 || :
    fail "upgraded from $previous, the schema differs from a fresh one's"
fi

# Upgraded and not: threads both ways, and peering both ways.
A reply "$ref" -m "After the upgrade" >/dev/null || fail "alice could not reply after the upgrade"
eventually "bob did not get alice's reply from $previous" holds "After the upgrade" ctl bob 18562 "$old/cpctl" show "$ref" --all --json
ctl bob 18562 "$old/cpctl" send alice "From the old release" -m "Mixed releases" >/dev/null || fail "bob could not send"
eventually "alice did not get bob's thread" holds "From the old release" A ls --json
start carol 18563 "$old/cpd"
start frank 18568 "$old/cpd"
befriend carol 18563 "$old/cpctl" alice 18561 "$new/cpctl"
befriend alice 18561 "$new/cpctl" frank 18568 "$old/cpctl"
# The request dave made before the upgrade.
id=$(A requests --json | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
[ -n "$id" ] || fail "upgraded: lost dave's request"
A approve "$id" >/dev/null || fail "upgraded: could not approve dave's request"
eventually "dave and alice did not become peers" holds '"status":"active"' ctl dave 18564 "$old/cpctl" peer show alice --json

# A migration ran: the copy cpd kept is what the earlier release reads,
# restored as README.md says, while the migrated data is refused.
if [ "$(stored "$work/alice/cp.db")" != "$before" ]; then
    copy=$work/alice/cp.db.schema-$before
    [ -f "$copy" ] || fail "migrated without keeping cp.db.schema-$before"
    stop "$alice_pid"
    cp -R "$work/alice" "$work/back"
    CP_DIR=$work/back "$old/cpd" --name alice --url http://127.0.0.1:18566 --listen 127.0.0.1:18566 >>"$work/back.log" 2>&1 &
    p=$!
    pids="$pids $p"
    i=0
    while kill -0 "$p" 2>/dev/null; do
        i=$((i + 1))
        [ "$i" -lt 50 ] || fail "$previous served data this build migrated"
        sleep 0.2
    done
    grep -q 'newer release' "$work/back.log" || fail "$previous did not refuse migrated data"
    rm -f "$work/back/cp.db-wal" "$work/back/cp.db-shm"
    cp "$work/back/cp.db.schema-$before" "$work/back/cp.db"
    start back 18566 "$old/cpd" alice
    # The last writes before the upgrade too: a copy made earlier would
    # lack them.
    holds '"status":"active"' ctl back 18566 "$old/cpctl" peer show bob --json ||
        fail "the copy restored does not hold the peer"
    holds "Seen before" ctl back 18566 "$old/cpctl" show "$ref" --all --json ||
        fail "the copy restored does not hold bob's reply"
    holds "upgrade test" ctl back 18566 "$old/cpctl" requests --json ||
        fail "the copy restored does not hold dave's request"
    start alice 18561 "$new/cpd"
fi

# The whole suite between this build and the previous release.
if [ -n "$conformance" ]; then
    CP_CONFORMANCE_URL=http://127.0.0.1:18562 CP_CONFORMANCE_TOKEN=$(cat "$work/bob/owner.token") \
        sh -c "cd '$root' && cargo nextest run --locked --package cp-conformance" ||
        fail "conformance failed against $previous"
fi

echo "upgrade tests passed: $previous to this build"
