#!/bin/sh
# Smoke-tests cpd-worker under `wrangler dev` (local workerd): the Worker
# reaches the Durable Object, which serves health and the event stream, and
# the cron trigger reaches the object.
set -eu

port=${WORKER_PORT:-18787}
base="http://127.0.0.1:$port"
wrangler=node_modules/.bin/wrangler
tmp=$(mktemp -d)
pid=

cleanup() {
    if [ -n "$pid" ]; then
        # wrangler re-executes itself and starts workerd: end the whole
        # group, and never block on a process that will not stop.
        kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
        i=0
        while kill -0 "$pid" 2>/dev/null && [ "$i" -lt 20 ]; do
            sleep 0.5
            i=$((i + 1))
        done
        kill -KILL -- "-$pid" 2>/dev/null || true
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

fail() {
    echo "worker smoke: $*" >&2
    echo "--- wrangler log" >&2
    tail -n 40 "$tmp/wrangler.log" >&2 || true
    exit 1
}

[ -x "$wrangler" ] || fail "run npm ci first"

# setsid makes wrangler a process group leader, so cleanup can end it and
# everything it starts. (Job control would too, but dash, CI's sh, has none
# in scripts.)
setsid "$wrangler" dev --port "$port" --ip 127.0.0.1 --test-scheduled \
    --persist-to "$tmp/state" >"$tmp/wrangler.log" 2>&1 &
pid=$!

i=0
until grep -q 'Ready on' "$tmp/wrangler.log"; do
    i=$((i + 1))
    [ "$i" -le 600 ] || fail "wrangler dev did not start"
    kill -0 "$pid" 2>/dev/null || fail "wrangler dev exited"
    sleep 0.5
done

health=$(curl -fsS --max-time 10 "$base/api/v1/health") || fail "health request failed"
case $health in
*'"status":"ok"'*) ;;
*) fail "unexpected health: $health" ;;
esac

# The stream stays open, so curl stops it after a few seconds.
stream=$(curl -s --max-time 3 "$base/api/v1/stream" || true)
case $stream in
'retry: 2000'*) ;;
*) fail "stream did not start with a retry line: $stream" ;;
esac

scheduled=$(curl -fsS --max-time 10 "$base/__scheduled?cron=*/15+*+*+*+*") ||
    fail "scheduled request failed"
case $scheduled in
*'Ran scheduled event'*) ;;
*) fail "unexpected scheduled result: $scheduled" ;;
esac

status=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "$base/internal/heartbeat")
[ "$status" = 404 ] || fail "internal path reachable from outside: $status"

echo "worker smoke: ok"
