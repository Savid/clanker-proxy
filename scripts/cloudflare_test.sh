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

# The Worker: answers /api/v1/me, and makes agent tokens. With a "stale"
# file, it refuses its storage until a deploy names new storage.
cat >"$work/worker.py" <<'PY'
import http.server, os, sys
work = sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def answer(self, body, status=200):
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(body.encode())
    def do_GET(self):
        secrets = os.path.join(work, "secrets.json")
        moved = os.path.exists(secrets) and "CP_STORAGE" in open(secrets).read()
        if os.path.exists(os.path.join(work, "stale")) and not moved:
            self.answer('{"status":503,"detail":"stored data is schema version 5, from an earlier release this one cannot migrate; start from empty storage"}', 503)
            return
        with open(os.path.join(work, "auth-seen"), "w") as f:
            f.write(self.headers.get("authorization", ""))
        self.answer('{"name":"tester"}')
    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", 0)))
        self.answer('{"name":"amp-inbox","token":"cpa_' + "A" * 43 + '"}')
    def do_PUT(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0))).decode()
        with open(os.path.join(work, "put"), "w") as f:
            f.write(self.path + " " + body)
        self.answer('{"name":"tester"}')
    def log_message(self, *a):
        pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
PY
port=18519
python3 "$work/worker.py" "$port" "$work" &
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
# An update of a deployment made before.
run_existing() { (export FAKE_EXISTING=1; run "$@"); }

sh "$script" --help | grep -q 'usage: cloudflare.sh' || fail "--help"
run --bogus && fail "an unknown option was accepted"
grep -q 'unknown option --bogus' "$work/out" || fail "unknown option message"
# Subshells keep these settings to one run: bash, macOS's sh, keeps an
# assignment made before a function call after the call returns.
# shellcheck disable=SC2030 # each run's settings stay in its subshell
(export FAKE_LOGGED_OUT=1 CLOUDFLARE_API_TOKEN=x; run --name tester) && fail "ran with a token Cloudflare rejects"
grep -q 'CLOUDFLARE_API_TOKEN is set in your environment, but Cloudflare finds no account for it' "$work/out" || fail "token message"
grep -q 'cloudflare.sh | env -u CLOUDFLARE_API_TOKEN sh$' "$work/out" || fail "token message: no rerun line"
# shellcheck disable=SC2031 # each run's settings stay in its subshell
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

# No alias unless one is given; one given is offered to peers.
[ ! -e "$work/put" ] || fail "set an alias nobody asked for"
rm -f "$token_file"
run --name tester --alias tess --worker cp-test --url "http://127.0.0.1:$port" || fail "setup with an alias failed"
grep -q 'owner tester (also tess)' "$work/out" || fail "alias: summary"
grep -q '^/api/v1/me/aliases {"aliases":\["tess"\]}$' "$work/put" || fail "alias: not offered"
run --name tester --alias tester --worker cp-test && fail "accepted the name as an alias"
grep -q 'alias tester is already your name' "$work/out" || fail "alias: own name message"
run --name tester --alias Bad_Alias --worker cp-test && fail "accepted an invalid alias"
grep -q 'alias Bad_Alias: lower-case' "$work/out" || fail "alias: invalid message"

# Amp: the prompt for the orb and the two values; with no terminal, the
# token appears as the file that holds it.
rm -f "$token_file"
run --name tester --worker cp-test --url "http://127.0.0.1:$port" --agent amp || fail "amp setup failed"
grep -q 'CP_URL  *http://127.0.0.1:18519' "$work/out" || fail "amp: no CP_URL"
grep -q 'CP_TOKEN  *secret, in .*cp-test.amp-inbox.token' "$work/out" || fail "amp: CP_TOKEN"
if grep -q 'CP_WEBHOOK_SECRET\|webhook add' "$work/out"; then fail "amp: a webhook step is left"; fi
grep -q 'Add the project Savid/clanker-proxy' "$work/out" || fail "amp: no prompt for the orb"
grep -q "on Amp's a1.tiny orbs" "$work/out" || fail "amp: the prompt names no orb size"
grep -q '^     https://github.com/Savid/clanker-proxy/blob/master/docs/amp.md\.$' "$work/out" || fail "amp: a local bundle's prompt names master's docs"
grep -q 'registers itself with your daemon' "$work/out" || fail "amp: no closing line"
grep -q "cpctl login http://127.0.0.1:18519 --token-file $token_file\$" "$work/out" || fail "amp: no cpctl login line for the owner"
if grep -q 'export CP_' "$work/out"; then fail "amp: export guidance is left"; fi
if grep -qE 'cpa_|cpo_' "$work/out"; then fail "a secret reached the output"; fi
grep -q '^cpa_' "$work/home/.config/clanker-proxy/cp-test.amp-inbox.token" || fail "amp: agent token not saved"

# Color only on a terminal or with FORCE_COLOR, and never with NO_COLOR.
# Lines to copy are colored around their text, never inside it.
esc=$(printf '\033')
if grep -q "$esc" "$work/out"; then fail "color in piped output"; fi
cp "$work/out" "$work/plain"
# run_colored <NO_COLOR> <option>...: a run with FORCE_COLOR set.
run_colored() { (no_color=$1; shift; export FORCE_COLOR=1 NO_COLOR="$no_color" LANG=C.UTF-8; run "$@"); }
rm -f "$token_file"
run_colored '' --name tester --worker cp-test --url "http://127.0.0.1:$port" --agent amp ||
    fail "FORCE_COLOR setup failed"
grep -q "$esc" "$work/out" || fail "FORCE_COLOR gave no color"
grep -q "^     ${esc}\[36mAdd the project Savid/clanker-proxy on Amp's a1.tiny orbs, then${esc}\[0m\$" "$work/out" ||
    fail "the Amp prompt is not paste-clean"
grep -q "^  ${esc}\[36mcpctl peer add tester http://127.0.0.1:18519${esc}\[0m\$" "$work/out" ||
    fail "the share line is not paste-clean"
rm -f "$token_file"
run_colored 1 --name tester --worker cp-test --url "http://127.0.0.1:$port" --agent amp ||
    fail "NO_COLOR setup failed"
cmp -s "$work/plain" "$work/out" || fail "NO_COLOR output is not the plain output"
run_colored '' --bogus && fail "an unknown option was accepted"
grep -q "^${esc}\[1;31m.*clanker-proxy:${esc}\[0m unknown option --bogus" "$work/out" || fail "FORCE_COLOR gave no colored error"

# With cpctl here, the setup logs it in, and the output says how to log in
# from elsewhere.
cat >"$work/cpctl" <<'FAKE'
#!/bin/sh
printf '%s\n' "$*" >>"$FAKE_DIR/cpctl-calls"
[ -z "${FAKE_LOGIN_FAILS:-}" ]
FAKE
chmod 755 "$work/cpctl"
# shellcheck disable=SC2030 # each run's settings stay in its subshell
run_cpctl() { (export CP_CPCTL="$work/cpctl"; run "$@"); }
rm -f "$token_file" "$work/cpctl-calls"
run_cpctl --name tester --worker cp-test --url "http://127.0.0.1:$port" --agent amp || fail "setup with cpctl failed"
grep -qx "login http://127.0.0.1:18519 --token-file $token_file" "$work/cpctl-calls" || fail "cpctl not logged in"
grep -q 'cpctl here is logged in' "$work/out" || fail "logged in: no line saying so"
grep -q '^     cpctl login http://127.0.0.1:18519$' "$work/out" || fail "logged in: no line for another machine"
if grep -q -- '--token-file' "$work/out"; then fail "logged in: still shows the token file"; fi
rm -f "$token_file"
(export FAKE_LOGIN_FAILS=1; run_cpctl --name tester --worker cp-test --url "http://127.0.0.1:$port") ||
    fail "setup with a failing login failed"
grep -q "cpctl login http://127.0.0.1:18519 --token-file $token_file\$" "$work/out" || fail "failed login: no login line"

before=$(cat "$token_file")
run_existing --worker cp-test --url "http://127.0.0.1:$port" || fail "update failed"
grep -q 'Updating cp-test; its settings stay as they are' "$work/out" || fail "update message"
grep -q 'cpctl peer add tester http://127.0.0.1:18519' "$work/out" || fail "no share line after an update"
grep -q 'Updated: http://127.0.0.1:18519 now runs' "$work/out" || fail "update: not checked with the token file"
[ "$(cat "$token_file")" = "$before" ] || fail "the update changed the owner token"

# An update logs in a cpctl that has no login, and leaves one it has alone.
login=$work/home/.config/clanker-proxy/login
# shellcheck disable=SC2031 # each run's settings stay in its subshell
update_cpctl() { (export CP_CPCTL="$work/cpctl"; run_existing "$@"); }
rm -f "$work/cpctl-calls" "$login"
update_cpctl --worker cp-test --url "http://127.0.0.1:$port" || fail "update with cpctl failed"
grep -qx "login http://127.0.0.1:18519 --token-file $token_file" "$work/cpctl-calls" || fail "update: cpctl not logged in"
printf 'url=https://elsewhere.example\ntoken=cpo_%s\n' "$(printf 'B%.0s' $(seq 43))" >"$login"
: >"$work/cpctl-calls"
update_cpctl --worker cp-test --url "http://127.0.0.1:$port" || fail "update with cpctl failed"
if grep -q '^login' "$work/cpctl-calls"; then fail "update: replaced another login"; fi

# Set up on another machine: no token file here, so the update is checked
# with cpctl's login for this URL, or not at all.
mv "$token_file" "$work/token.away"
run_existing --worker cp-test --url "http://127.0.0.1:$port" || fail "unchecked update failed"
grep -q 'Deployed local to http://127.0.0.1:18519\.' "$work/out" || fail "unchecked update: says it was checked"
if grep -q 'now runs\|cpctl peer add' "$work/out"; then fail "unchecked update: claims what it did not check"; fi
printf 'url=http://127.0.0.1:18519\ntoken=%s\n' "$(cat "$work/token.away")" >"$login"
run_existing --worker cp-test --url "http://127.0.0.1:$port" || fail "update with a login failed"
grep -q 'Updated: http://127.0.0.1:18519 now runs' "$work/out" || fail "update: not checked with the login"
[ "$(cat "$work/auth-seen")" = "Bearer $(cat "$work/token.away")" ] || fail "update: checked without the login's token"
grep -q 'cpctl peer add tester http://127.0.0.1:18519' "$work/out" || fail "update with a login: no share line"
if grep -q 'cpo_' "$work/out"; then fail "the login's token reached the output"; fi
rm -f "$login"
mv "$work/token.away" "$token_file"

# A release that cannot read the daemon's data: refused without --start-over,
# then a fresh setup on new storage that keeps the owner token.
: >"$work/stale"
rm -f "$work/secrets.json"
run_existing --worker cp-test --url "http://127.0.0.1:$port" && fail "updated over data it cannot read"
grep -q 'rerun with --start-over' "$work/out" || fail "no start-over advice"
run_existing --worker cp-test --url "http://127.0.0.1:$port" --name tester --start-over || fail "start over failed"
grep -q 'Starting cp-test over on empty storage' "$work/out" || fail "start over message"
grep -q 'running at http://127.0.0.1:18519, owner tester' "$work/out" || fail "no summary after starting over"
grep -q '"CP_STORAGE":"owner-[0-9]*"' "$work/secrets.json" || fail "start over sent no new storage name"
[ "$(cat "$token_file")" = "$before" ] || fail "starting over changed the owner token"
rm "$work/stale"

echo "cloudflare setup tests passed"
