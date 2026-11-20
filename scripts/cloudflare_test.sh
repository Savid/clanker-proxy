#!/bin/sh
# Tests scripts/cloudflare.sh offline, on Linux and macOS: a fake wrangler
# records what it is asked, and a local HTTP server stands in for the Worker.
set -eu

script=$(cd "$(dirname "$0")" && pwd)/cloudflare.sh
work=$(mktemp -d "${TMPDIR:-/tmp}/clanker-proxy-cf-test.XXXXXXXX")
server=
cleanup() {
    [ -z "$server" ] || kill "$server" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup 0
trap 'exit 1' HUP INT TERM

fail() {
    printf 'cloudflare_test: %s\n' "$*" >&2
    [ ! -f "$work/out" ] || sed 's/^/  | /' "$work/out" >&2
    exit 1
}

# A Worker bundle with nothing to install.
mkdir -p "$work/bundle"
printf '{"name":"fake","private":true}\n' >"$work/bundle/package.json"
printf '{"name":"fake","lockfileVersion":3,"requires":true,"packages":{"":{"name":"fake"}}}\n' >"$work/bundle/package-lock.json"
tar -czf "$work/bundle.tar.gz" -C "$work/bundle" package.json package-lock.json

cat >"$work/wrangler" <<'FAKE'
#!/bin/sh
printf '%s\n' "$*" >>"$FAKE_DIR/calls"
case $1 in
    whoami) [ -n "${FAKE_LOGGED_OUT:-}" ] && echo "You are not authenticated" || echo "| Account ID | abc |" ;;
    secret) [ -n "${FAKE_EXISTING:-}" ] && echo '[{"name":"CP_OWNER_TOKEN_HASH","type":"secret_text"}]' || echo '[]' ;;
    deploy)
        while [ "$#" -gt 0 ]; do
            [ "$1" = --secrets-file ] && cp "$2" "$FAKE_DIR/secrets.json"
            shift
        done
        echo "Deployed"
        echo "  https://cp-test.fake.workers.dev" ;;
    *) exit 1 ;;
esac
FAKE
chmod 755 "$work/wrangler"

# The Worker: answers /api/v1/me, and makes agent tokens.
cat >"$work/worker.py" <<'PY'
import http.server, sys
class H(http.server.BaseHTTPRequestHandler):
    def answer(self, body):
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(body.encode())
    def do_GET(self):
        self.answer('{"name":"tester"}')
    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", 0)))
        self.answer('{"name":"amp-inbox","token":"cpa_' + "A" * 43 + '"}')
    def log_message(self, *a):
        pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
PY
port=18519
python3 "$work/worker.py" "$port" &
server=$!
i=0
until curl -fsS "http://127.0.0.1:$port/api/v1/me" >/dev/null 2>&1; do
    i=$((i + 1))
    [ "$i" -lt 50 ] || fail "test server did not start"
    sleep 0.2
done

run() {
    HOME=$work/home XDG_CONFIG_HOME='' XDG_DATA_HOME='' CP_NO_TTY=1 \
        CP_WRANGLER=$work/wrangler FAKE_DIR=$work \
        sh "$script" --bundle "$work/bundle.tar.gz" "$@" >"$work/out" 2>&1
}
token_file=$work/home/.config/clanker-proxy/cp-test.token

sh "$script" --help | grep -q 'usage: cloudflare.sh' || fail "--help"
run --bogus && fail "an unknown option was accepted"
grep -q 'unknown option --bogus' "$work/out" || fail "unknown option message"
# Subshells keep these settings to one run: bash, macOS's sh, keeps an
# assignment made before a function call after the call returns.
(export FAKE_LOGGED_OUT=1; run --name tester) && fail "ran without a Cloudflare login"
grep -q 'not logged in to Cloudflare' "$work/out" || fail "login message"
run --worker cp-test && fail "ran without --name and no terminal"
grep -q 'pass --name' "$work/out" || fail "missing --name message"
run --name Bad_Name --worker cp-test && fail "accepted an invalid name"
run --name tester --worker cp-test --agent maybe && fail "accepted an unknown --agent"
grep -q -- '--agent takes amp, local or none' "$work/out" || fail "--agent message"

: >"$work/calls"
run --name tester --worker cp-test --url "http://127.0.0.1:$port" || fail "first setup failed"
grep -q 'running at http://127.0.0.1:18519, owner tester' "$work/out" || fail "summary"
grep -q 'cpctl peer add tester http://127.0.0.1:18519' "$work/out" || fail "no share line"
if grep -q 'cpo_' "$work/out"; then fail "the owner token reached the output"; fi
[ -n "$(find "$token_file" -perm 0600)" ] || fail "token file is not private"
grep -Eq '^cpo_[A-Za-z0-9_-]{43}$' "$token_file" || fail "malformed owner token"
[ "$(grep -c '^deploy' "$work/calls")" = 2 ] || fail "want two deploys"
grep -q '^deploy --name cp-test --secrets-file ' "$work/calls" || fail "settings not sent with a deploy"
TOKEN=$(cat "$token_file") node -e '
const s = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
const hash = require("crypto").createHash("sha256").update(process.env.TOKEN).digest("hex");
const ok = s.CP_NAME === "tester" && s.CP_URL === "http://127.0.0.1:18519" && s.CP_OWNER_TOKEN_HASH === hash
  && !JSON.stringify(s).includes(process.env.TOKEN);
process.exit(ok ? 0 : 1);
' "$work/secrets.json" || fail "settings file does not match"

# Amp: the prompt for the orb and the two values; with no terminal, the
# token appears as the file that holds it.
rm -f "$token_file"
run --name tester --worker cp-test --url "http://127.0.0.1:$port" --agent amp || fail "amp setup failed"
grep -q 'CP_URL  *http://127.0.0.1:18519' "$work/out" || fail "amp: no CP_URL"
grep -q 'CP_TOKEN  *secret, in .*cp-test.amp-inbox.token' "$work/out" || fail "amp: CP_TOKEN"
if grep -q 'CP_WEBHOOK_SECRET\|webhook add' "$work/out"; then fail "amp: a webhook step is left"; fi
grep -q 'Start a thread to be my clanker-proxy inbox' "$work/out" || fail "amp: no prompt for the orb"
grep -q 'registers itself with your daemon' "$work/out" || fail "amp: no closing line"
grep -q 'export CP_URL=http://127.0.0.1:18519 CP_TOKEN=' "$work/out" || fail "amp: no cpctl line for the owner"
if grep -qE 'cpa_|cpo_' "$work/out"; then fail "a secret reached the output"; fi
grep -q '^cpa_' "$work/home/.config/clanker-proxy/cp-test.amp-inbox.token" || fail "amp: agent token not saved"

before=$(cat "$token_file")
(export FAKE_EXISTING=1; run --worker cp-test --url "http://127.0.0.1:$port") || fail "update failed"
grep -q 'Updating cp-test; its settings stay as they are' "$work/out" || fail "update message"
grep -q 'cpctl peer add tester http://127.0.0.1:18519' "$work/out" || fail "no share line after an update"
[ "$(cat "$token_file")" = "$before" ] || fail "the update changed the owner token"

echo "cloudflare setup tests passed"
