#!/bin/sh
# Tests upgrading from the latest release (or CP_PREVIOUS, a tag) to this
# build: daemons of that release make real data, one is upgraded in place,
# and it keeps working with peers still on that release. Then the
# conformance suite runs between this build and that release. Downloads the
# release; needs cargo and cargo-nextest.
set -eu

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
if [ -n "${CP_PREVIOUS:-}" ]; then
    base=$repo/releases/download/$CP_PREVIOUS
else
    base=$repo/releases/latest/download
fi
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1
}
archive=clanker-proxy_$asset.tar.gz
curl -fsSL -o "$work/$archive" "$base/$archive" || fail "download $base/$archive"
curl -fsSL -o "$work/checksums.txt" "$base/checksums.txt" || fail "download $base/checksums.txt"
want=$(sed -n "s/^\([0-9a-f]\{64\}\)  $archive\$/\1/p" "$work/checksums.txt")
if [ -z "$want" ] || [ "$(sha256 "$work/$archive")" != "$want" ]; then
    fail "$archive does not match checksums.txt"
fi
mkdir "$work/old"
tar -xzf "$work/$archive" -C "$work/old"
old=$work/old
previous=$("$old/cpctl" --version)

(cd "$root" && cargo build --quiet --locked -p cpd -p cpctl) || fail "build"
new=${CARGO_TARGET_DIR:-$root/target}/debug

# start <name> <port> <cpd>: a daemon in its own directory, on loopback;
# $started is its process.
start() {
    CP_DIR=$work/$1 "$3" --name "$1" --url "http://127.0.0.1:$2" --listen "127.0.0.1:$2" >>"$work/$1.log" 2>&1 &
    started=$!
    pids="$pids $started"
    i=0
    until curl -fsS "http://127.0.0.1:$2/api/v1/health" >/dev/null 2>&1; do
        i=$((i + 1))
        [ "$i" -lt 50 ] || fail "$1 did not start"
        sleep 0.2
    done
}

# ctl <name> <port> <cpctl> <args>...: cpctl as that daemon's owner.
ctl() {
    d=$1 port=$2 cli=$3
    shift 3
    HOME=$work XDG_CONFIG_HOME=$work/config CP_URL="http://127.0.0.1:$port" \
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

# Data made by the previous release: a peer, a thread both wrote in, an
# agent token and a webhook.
start alice 18561 "$old/cpd"
alice_pid=$started
start bob 18562 "$old/cpd"
befriend alice 18561 "$old/cpctl" bob 18562 "$old/cpctl"
ref=$(ctl alice 18561 "$old/cpctl" send bob "Upgrade test" -m "Before the upgrade" --json |
    sed -n 's/^{"id":"\([^"]*\)".*/\1/p')
[ -n "$ref" ] || fail "alice could not send"
eventually "bob did not get the thread" holds "$ref" ctl bob 18562 "$old/cpctl" ls --json
ctl bob 18562 "$old/cpctl" reply "$ref" -m "Seen before" >/dev/null || fail "bob could not reply"
eventually "alice did not get bob's reply" holds "Seen before" ctl alice 18561 "$old/cpctl" show "$ref" --all --json
ctl alice 18561 "$old/cpctl" token add agent -o "$work/agent.token" >/dev/null || fail "token add"
ctl alice 18561 "$old/cpctl" webhook add hook http://127.0.0.1:9/hook >/dev/null || fail "webhook add"

# Alice upgrades in place.
kill "$alice_pid"
wait "$alice_pid" 2>/dev/null || true
start alice 18561 "$new/cpd"
A() { ctl alice 18561 "$new/cpctl" "$@"; }
holds alice A me || fail "upgraded: lost the owner's name"
holds '"status":"active"' A peer show bob --json || fail "upgraded: lost the peer"
holds "Before the upgrade" A show "$ref" --all --json || fail "upgraded: lost alice's message"
holds "Seen before" A show "$ref" --all --json || fail "upgraded: lost bob's message"
holds agent A token ls || fail "upgraded: lost the agent token"
holds hook A webhook ls || fail "upgraded: lost the webhook"
holds "$ref" env CP_TOKEN="$(cat "$work/agent.token")" HOME="$work" XDG_CONFIG_HOME="$work/config" \
    CP_URL=http://127.0.0.1:18561 "$new/cpctl" ls --json || fail "upgraded: the agent token stopped working"

# Upgraded and not: threads both ways, and peering both ways.
A reply "$ref" -m "After the upgrade" >/dev/null || fail "alice could not reply after the upgrade"
eventually "bob did not get alice's reply from $previous" holds "After the upgrade" ctl bob 18562 "$old/cpctl" show "$ref" --all --json
ctl bob 18562 "$old/cpctl" send alice "From the old release" -m "Mixed releases" >/dev/null || fail "bob could not send"
eventually "alice did not get bob's thread" holds "From the old release" A ls --json
start carol 18563 "$old/cpd"
start dave 18564 "$old/cpd"
befriend carol 18563 "$old/cpctl" alice 18561 "$new/cpctl"
befriend alice 18561 "$new/cpctl" dave 18564 "$old/cpctl"

# The whole suite between this build and the previous release.
CP_CONFORMANCE_URL=http://127.0.0.1:18562 CP_CONFORMANCE_TOKEN=$(cat "$work/bob/owner.token") \
    sh -c "cd '$root' && cargo nextest run --locked --package cp-conformance" ||
    fail "conformance failed against $previous"

echo "upgrade tests passed: $previous to this build"
