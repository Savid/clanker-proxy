#!/bin/sh
# Tests cpd-worker under `wrangler dev` (local workerd): the Worker turns
# away what it can, the Durable Object serves the API and the event stream,
# the cron trigger reaches it, and then the conformance suite runs a native
# daemon against it.
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

# A throwaway owner token: cpo_ and 32 random bytes in base64url.
token="cpo_$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n')"

# setsid makes wrangler a process group leader, so cleanup can end it and
# everything it starts. (Job control would too, but dash, CI's sh, has none
# in scripts.)
setsid "$wrangler" dev --port "$port" --ip 127.0.0.1 --test-scheduled \
    --persist-to "$tmp/state" --var CP_NAME:worker --var "CP_URL:$base" \
    --var "CP_OWNER_TOKEN:$token" >"$tmp/wrangler.log" 2>&1 &
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

me=$(curl -fsS --max-time 10 -H "Authorization: Bearer $token" "$base/api/v1/me") ||
    fail "me request failed"
case $me in
*'"name":"worker"'*) ;;
*) fail "unexpected me: $me" ;;
esac

# Turned away by the Worker without a token, before the object wakes.
status=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "$base/api/v1/threads")
[ "$status" = 401 ] || fail "threads without a token: $status"

status=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "$base/openapi.json")
[ "$status" = 200 ] || fail "openapi.json: $status"

# The stream stays open, so curl stops it after a few seconds.
stream=$(curl -s --max-time 3 -H "Authorization: Bearer $token" "$base/api/v1/stream" || true)
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

CP_CONFORMANCE_URL=$base CP_CONFORMANCE_TOKEN=$token \
    cargo nextest run --locked --package cp-conformance ||
    fail "conformance suite failed against the worker"

echo "worker smoke: ok"
