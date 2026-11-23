#!/bin/sh
# Sets up, or updates, clanker-proxy on Cloudflare: downloads the prebuilt
# Worker from a release, deploys it with wrangler, and stores its settings.
#
#   curl -fsSL https://github.com/Savid/clanker-proxy/releases/latest/download/cloudflare.sh | sh
#
# On a terminal it asks what it needs. Without one (an agent running it),
# pass the answers: --name <owner> [--worker <name>] [--agent amp|local|none].
# Secrets are shown only on a terminal, never on standard output, so they
# stay out of logs and agent transcripts; files hold them either way.
set -eu

repo=https://github.com/Savid/clanker-proxy
config=${XDG_CONFIG_HOME:-$HOME/.config}/clanker-proxy
data=${XDG_DATA_HOME:-$HOME/.local/share}/clanker-proxy

usage() {
    cat <<'EOF'
usage: cloudflare.sh [options]

Deploys clanker-proxy to Cloudflare, or updates a deployment made before.

  --name <owner>     your name: lower-case letters, digits, single hyphens
  --alias <name>     another name peers can call you (default: none)
  --worker <name>    the Worker's name (default clanker); its URL is
                     https://<name>.<your-subdomain>.workers.dev
  --url <url>        the URL peers use, if not the workers.dev one
  --token-file <f>   use the owner token in this file
  --agent <how>      how your agents run: amp (Amp orbs: also makes the agent
                     token an Amp inbox needs), local (on this machine), or
                     none (decide later)
  --bundle <file>    deploy this Worker bundle instead of downloading one
  --start-over       when this release cannot read the daemon's data (one
                     that changed storage), start on empty storage: its
                     peers, threads and agent tokens stay behind

Environment: CP_VERSION picks a release (default: the latest);
CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID log in without a browser.
Needs Node.js 20+ and curl.
EOF
}

say() { printf '%s\n' "$*"; }
fail() {
    printf 'clanker-proxy: %s\n' "$*" >&2
    exit 1
}

# A terminal to ask on and show secrets to, even though this script
# arrives on standard input.
# CP_NO_TTY and CP_WRANGLER exist for scripts/cloudflare_test.sh.
tty=
if [ -z "${CP_NO_TTY:-}" ] && (: </dev/tty >/dev/tty) 2>/dev/null; then
    tty=/dev/tty
fi

# ask <prompt> <default>: the answer, or the default without a terminal.
ask() {
    if [ -z "$tty" ]; then
        printf '%s\n' "$2"
        return
    fi
    if [ -n "$2" ]; then
        printf '%s [%s]: ' "$1" "$2" >"$tty"
    else
        printf '%s: ' "$1" >"$tty"
    fi
    answer=
    IFS= read -r answer <"$tty" || true
    printf '%s\n' "${answer:-$2}"
}

# Shows a secret on the terminal only.
show() {
    if [ -n "$tty" ]; then
        printf '%s\n' "$*" >"$tty"
    fi
}

valid_name() { # valid_name <value> <max length>
    [ "${#1}" -le "$2" ] && printf '%s' "$1" | grep -Eq '^[a-z0-9]+(-[a-z0-9]+)*$'
}

name='' alias='' worker='' url='' token_file='' agent='' bundle='' start_over='' storage=''
while [ "$#" -gt 0 ]; do
    case $1 in
        -h | --help) usage; exit 0 ;;
        --start-over) start_over=yes; shift; continue ;;
        --name | --alias | --worker | --url | --token-file | --bundle | --agent) ;;
        *) fail "unknown option $1 (see --help)" ;;
    esac
    [ "$#" -ge 2 ] || fail "$1 needs a value (see --help)"
    case $1 in
        --name) name=$2 ;;
        --alias) alias=$2 ;;
        --worker) worker=$2 ;;
        --url) url=$2 ;;
        --token-file) token_file=$2 ;;
        --bundle) bundle=$2 ;;
        --agent) agent=$2 ;;
    esac
    shift 2
done

case $agent in
    '' | amp | local | none) ;;
    *) fail "--agent takes amp, local or none" ;;
esac

for tool in node npm curl tar; do
    command -v "$tool" >/dev/null 2>&1 || fail "needs $tool (Node.js 20+ and curl)"
done
node -e 'process.exit(+process.versions.node.split(".")[0] >= 20 ? 0 : 1)' ||
    fail "needs Node.js 20 or later"

tmp=$(mktemp -d "${TMPDIR:-/tmp}/clanker-proxy-cf.XXXXXXXX")
trap 'rm -rf "$tmp"' 0
trap 'exit 1' HUP INT TERM

# ---- the Worker ----

dir=$data/worker
mkdir -p "$dir" "$config"
if [ -n "$bundle" ]; then
    cp "$bundle" "$tmp/worker.tar.gz" || fail "cannot read $bundle"
    version=local
else
    version=${CP_VERSION:-}
    if [ -z "$version" ]; then
        latest=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$repo/releases/latest") ||
            fail "cannot find the latest release"
        version=${latest##*/}
    fi
    printf '%s\n' "$version" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$' || fail "not a release tag: $version"
    base=$repo/releases/download/$version
    curl -fsSL -o "$tmp/worker.tar.gz" "$base/clanker-proxy_worker.tar.gz" ||
        fail "cannot download the Worker for $version"
    curl -fsSL -o "$tmp/checksums.txt" "$base/checksums.txt" || fail "cannot download checksums.txt"
    want=$(awk '$2 == "clanker-proxy_worker.tar.gz" { print $1 }' "$tmp/checksums.txt")
    if command -v sha256sum >/dev/null 2>&1; then
        have=$(sha256sum "$tmp/worker.tar.gz")
    else
        have=$(shasum -a 256 "$tmp/worker.tar.gz")
    fi
    if [ -z "$want" ] || [ "${have%% *}" != "$want" ]; then
        fail "checksum mismatch; nothing was deployed"
    fi
fi
say "clanker-proxy $version for Cloudflare"
rm -rf "$dir/build"
tar -xzf "$tmp/worker.tar.gz" -C "$dir" || fail "cannot unpack the Worker"
(cd "$dir" && npm ci --no-audit --no-fund --loglevel=error >"$tmp/npm.log" 2>&1) || {
    tail -n 20 "$tmp/npm.log" >&2
    fail "npm ci failed"
}

wr() {
    (cd "$dir" && "${CP_WRANGLER:-./node_modules/.bin/wrangler}" "$@")
}

# ---- Cloudflare login ----

if ! wr whoami 2>/dev/null | grep -q 'Account ID'; then
    if [ -z "$tty" ] || [ -n "${CLOUDFLARE_API_TOKEN:-}" ]; then
        fail "not logged in to Cloudflare: run 'npx wrangler login', or set CLOUDFLARE_API_TOKEN (Edit Cloudflare Workers template) and CLOUDFLARE_ACCOUNT_ID"
    fi
    say "Log in to Cloudflare in the browser that opens."
    # shellcheck disable=SC2094 # the terminal, read and written by design
    wr login <"$tty" >"$tty" 2>&1 || fail "Cloudflare login failed"
fi

# ---- which account ----

# Wrangler cannot pick among several accounts while this script reads its
# output, so the choice is made here.
if [ -z "${CLOUDFLARE_ACCOUNT_ID:-}" ]; then
    # shellcheck disable=SC2016 # JavaScript, not shell: nothing to expand.
    wr whoami --json 2>/dev/null | node -e '
let s = ""
process.stdin.on("data", (d) => (s += d)).on("end", () => {
    try {
        for (const a of JSON.parse(s).accounts ?? []) console.log(`${a.id} ${a.name}`)
    } catch {}
})' >"$tmp/accounts"
    count=$(wc -l <"$tmp/accounts" | tr -d ' ')
    if [ "$count" = 1 ]; then
        CLOUDFLARE_ACCOUNT_ID=$(cut -d' ' -f1 "$tmp/accounts")
    elif [ "$count" -gt 1 ]; then
        [ -n "$tty" ] || fail "several Cloudflare accounts: set CLOUDFLARE_ACCOUNT_ID to the one to use"
        printf '\nWhich Cloudflare account?\n' >"$tty"
        awk '{ id = $1; $1 = ""; printf "  %d)%s\n", NR, $0 }' "$tmp/accounts" >"$tty"
        pick=$(ask "Choose" 1)
        CLOUDFLARE_ACCOUNT_ID=$(sed -n "${pick}p" "$tmp/accounts" | cut -d' ' -f1)
        [ -n "$CLOUDFLARE_ACCOUNT_ID" ] || fail "no account $pick"
    fi
    export CLOUDFLARE_ACCOUNT_ID
fi

# The account's workers.dev subdomain, to show the URL a name gives. The
# token goes from wrangler to the request through a pipe, never an argument
# or the screen. Empty when it cannot tell; then only the prompt is vaguer.
subdomain=
if [ -n "${CLOUDFLARE_ACCOUNT_ID:-}" ]; then
    # shellcheck disable=SC2016 # JavaScript, not shell: nothing to expand.
    subdomain=$(wr auth token --json 2>/dev/null | node -e '
let s = ""
process.stdin.on("data", (d) => (s += d)).on("end", async () => {
    try {
        const { token } = JSON.parse(s)
        const r = await fetch(`https://api.cloudflare.com/client/v4/accounts/${process.argv[1]}/workers/subdomain`, {
            headers: { authorization: `Bearer ${token}` },
            signal: AbortSignal.timeout(10000),
        })
        const j = await r.json()
        if (j.success && /^[a-z0-9-]+$/.test(j.result?.subdomain ?? "")) console.log(j.result.subdomain)
    } catch {}
})' "$CLOUDFLARE_ACCOUNT_ID" || true)
fi

# ---- what to deploy ----

if [ -n "$subdomain" ]; then
    worker=${worker:-$(ask "Worker name (its URL will be https://<name>.$subdomain.workers.dev)" clanker)}
else
    worker=${worker:-$(ask "Worker name (its URL is https://<name>.<your-subdomain>.workers.dev)" clanker)}
fi
valid_name "$worker" 63 || fail "Worker name $worker: lower-case letters, digits and single hyphens"

existing=
if wr secret list --name "$worker" 2>/dev/null | grep -q '"CP_OWNER_TOKEN_HASH"'; then
    existing=yes
fi

# Deploys, waiting out the Cloudflare API's lag right after a first upload.
deploy() {
    i=0
    while :; do
        i=$((i + 1))
        if wr deploy --name "$worker" "$@" >"$tmp/deploy.log" 2>&1; then
            return
        fi
        if [ "$i" -lt 8 ] && grep -qE 'code: (10007|10079)' "$tmp/deploy.log"; then
            say "Cloudflare is still registering the new Worker; trying again in 20 seconds."
            sleep 20
            continue
        fi
        tail -n 20 "$tmp/deploy.log" >&2
        if grep -q '10079' "$tmp/deploy.log"; then
            fail "deploy failed: delete the half-made Worker (npx wrangler delete --name $worker), wait a minute, and run this again"
        fi
        if grep -qi 'workers.dev subdomain' "$tmp/deploy.log"; then
            fail "choose your workers.dev subdomain once at https://dash.cloudflare.com (Workers & Pages), then run this again"
        fi
        fail "deploy failed"
    done
}

# What to send someone so they can connect: the command they run.
share() {
    say ""
    say "Share this with anyone you want to connect with:"
    say "  cpctl peer add $1 $2"
    say "They run it and send you the code it prints. If it matches the one in"
    say "cpctl requests, approve it: cpctl approve <id>."
}

if [ -n "$existing" ]; then
    say "Updating $worker; its settings stay as they are."
    deploy
    found=$(grep -Eo 'https://[a-z0-9.-]+\.workers\.dev' "$tmp/deploy.log" | head -n 1 || true)
    url=${url:-$found}
    # With the owner token here, the update is checked, and the share line
    # can name the owner.
    saved=${token_file:-$config/$worker.token}
    stale='' owner=''
    if [ -s "$saved" ]; then
        (umask 077 && printf 'Authorization: Bearer %s\n' "$(cat "$saved")" >"$tmp/auth")
        # Until the new release answers. A release that changed storage
        # refuses the daemon's earlier data.
        i=0
        while :; do
            curl -sS -H @"$tmp/auth" "$url/api/v1/me" >"$tmp/me.json" 2>/dev/null || :
            if grep -q 'start from empty storage' "$tmp/me.json"; then
                stale=yes
                break
            fi
            if grep -q '"name":' "$tmp/me.json" && { [ "$version" = local ] || grep -q "\"version\":\"$version\"" "$tmp/me.json"; }; then
                owner=$(sed -n 's/.*"name":"\([^"]*\)".*/\1/p' "$tmp/me.json")
                break
            fi
            i=$((i + 1))
            [ "$i" -lt 30 ] || break
            sleep 2
        done
    fi
    if [ -z "$stale" ]; then
        say "Updated: $url now runs $version."
        [ -z "$alias" ] || say "Aliases change with: cpctl alias $alias"
        [ -z "$owner" ] || share "$owner" "$url"
        exit 0
    fi
    if [ -z "$start_over" ] && [ -n "$tty" ] &&
        [ "$(ask "$worker's data is from an earlier release, which $version cannot read. Start over on empty storage? Its peers, threads and agent tokens stay behind (y/n)" n)" = y ]; then
        start_over=yes
    fi
    [ -n "$start_over" ] ||
        fail "$worker's data is from an earlier release, which $version cannot read; rerun with --start-over to start on empty storage (its peers, threads and agent tokens stay behind, and you peer again)"
    # A new name for the Durable Object; the old one keeps its data, unreachable.
    storage=owner-$(date +%s)
    say "Starting $worker over on empty storage."
fi

[ -n "$name" ] || [ -n "$tty" ] || fail "pass --name <your name>: there is no terminal to ask on"
default_name=$(printf '%s' "${USER:-me}" | tr 'A-Z_.' 'a-z--' | tr -cd 'a-z0-9-')
name=${name:-$(ask "Your name, as peers will see it" "${default_name:-me}")}
valid_name "$name" 39 || fail "name $name: lower-case letters, digits and single hyphens, at most 39"
alias=${alias:-$(ask "Another name peers can call you (optional)" '')}
if [ -n "$alias" ]; then
    valid_name "$alias" 39 || fail "alias $alias: lower-case letters, digits and single hyphens, at most 39"
    [ "$alias" != "$name" ] || fail "alias $alias is already your name"
fi

# The owner token: a file given, the one saved by an earlier run, one
# entered, or a new one.
token_file=${token_file:-$config/$worker.token}
if [ ! -s "$token_file" ]; then
    choice=$(ask "Owner token: (g)enerate a new one or (e)nter your own" g)
    (umask 077 && : >"$token_file")
    if [ "$choice" = e ] && [ -n "$tty" ]; then
        printf 'Owner token (input hidden): ' >"$tty"
        stty -echo <"$tty"
        IFS= read -r entered <"$tty" || entered=
        stty echo <"$tty"
        printf '\n' >"$tty"
        printf '%s\n' "$entered" | grep -Eq '^cpo_[A-Za-z0-9_-]{43}$' ||
            fail "an owner token is cpo_ and 43 base64url characters"
        printf '%s\n' "$entered" >"$token_file"
    else
        printf 'cpo_%s\n' "$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n')" >"$token_file"
    fi
fi
grep -Eq '^cpo_[A-Za-z0-9_-]{43}$' "$token_file" || fail "$token_file does not hold an owner token"

say "Deploying $worker..."
deploy
if [ -z "$url" ]; then
    url=$(grep -Eo 'https://[a-z0-9.-]+\.workers\.dev' "$tmp/deploy.log" | head -n 1 || true)
    [ -n "$url" ] || fail "deployed, but wrangler printed no workers.dev URL; run again with --url <the Worker's URL>"
fi

# All three settings go out in one deploy, so no version ever runs with only
# some of them. The Worker gets only the owner token's SHA-256: the token
# stays in the file on this machine.
(umask 077 && NAME=$name URL=$url STORAGE=$storage TOKEN=$(cat "$token_file") node -e '
const hash = require("crypto").createHash("sha256").update(process.env.TOKEN.trim()).digest("hex");
const storage = process.env.STORAGE ? { CP_STORAGE: process.env.STORAGE } : {};
process.stdout.write(JSON.stringify({ CP_NAME: process.env.NAME, CP_URL: process.env.URL, CP_OWNER_TOKEN_HASH: hash, ...storage }));
' >"$tmp/secrets.json")
say "Storing settings..."
deploy --secrets-file "$tmp/secrets.json"

# The header goes through a file, so the token never appears in a process list.
(umask 077 && printf 'Authorization: Bearer %s\n' "$(cat "$token_file")" >"$tmp/auth")
i=0
until curl -fsS -H @"$tmp/auth" "$url/api/v1/me" >"$tmp/me.json" 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -lt 30 ] || fail "$url does not answer yet; check it with: curl $url/api/v1/health"
    sleep 2
done

# Names are lower-case letters, digits and hyphens, so safe in JSON as is.
called=$name
if [ -n "$alias" ]; then
    if curl -fsS -X PUT -H @"$tmp/auth" -H 'content-type: application/json' \
        -d "{\"aliases\":[\"$alias\"]}" "$url/api/v1/me/aliases" >/dev/null 2>&1; then
        called="$name (also $alias)"
    else
        say "Could not set the alias; set it later with: cpctl alias $alias"
    fi
fi

say ""
say "clanker-proxy is running at $url, owner $called."
say "Owner token saved to $token_file (only you can read it)."
show ""
show "OWNER TOKEN: full control of your daemon. Save it in your password"
show "manager, and never give it to an agent:"
show "  $(cat "$token_file")"

if [ -z "$agent" ]; then
    if [ -n "$tty" ]; then
        printf '\nHow do your agents run?\n  1) Amp (orbs)\n  2) On this machine: Claude Code, Codex, any CLI agent\n  3) Not sure yet\n' >"$tty"
        case $(ask "Choose" 1) in
            1) agent=amp ;;
            2) agent=local ;;
            *) agent=none ;;
        esac
    else
        agent=none
    fi
fi

# ---- cpctl on this machine ----

# The owner approves peers and manages tokens with cpctl here, so it is kept
# at the Worker's release. Not fatal: the Worker is up either way.
bin=${CP_INSTALL_DIR:-$HOME/.local/bin}
cli=
if [ "$version" != local ]; then
    if [ "$("$bin/cpctl" --version 2>/dev/null || true)" = "cpctl $version" ]; then
        cli=$bin/cpctl
    elif curl -fsSL -o "$tmp/install.sh" "$base/install.sh" &&
        CP_VERSION=$version CP_INSTALL_DIR=$bin sh "$tmp/install.sh" >"$tmp/install.log" 2>&1; then
        cli=$bin/cpctl
        say "Installed cpctl $version in $bin."
    else
        say "Could not install cpctl here; the install line below does it."
    fi
    if [ -n "$cli" ]; then
        case :${PATH:-}: in
            *:"$bin":*) ;;
            *) say "Add $bin to your PATH." ;;
        esac
    fi
fi

# Prints a line holding a secret: on a terminal with the value, otherwise
# with where the value is saved, so it stays out of logs and transcripts.
secret_line() { # secret_line <label> <file>
    if [ -n "$tty" ]; then
        printf '     %-9s %s   secret\n' "$1" "$(cat "$2")" >"$tty"
    else
        printf '     %-9s secret, in %s\n' "$1" "$2"
    fi
}

use_here() {
    [ -n "$cli" ] || say "     curl -fsSL $repo/releases/latest/download/install.sh | sh"
    say "     export CP_URL=$url CP_TOKEN=\"\$(cat $token_file)\""
}

if [ "$agent" = amp ]; then
    agent_file=$config/$worker.amp-inbox.token
    if curl -fsS -H @"$tmp/auth" -H 'content-type: application/json' -d '{"name":"amp-inbox"}' \
        "$url/api/v1/agent-tokens" >"$tmp/agent.json" 2>/dev/null; then
        (umask 077 && sed -n 's/.*"token":"\([^"]*\)".*/\1/p' "$tmp/agent.json" >"$agent_file")
    elif [ ! -s "$agent_file" ]; then
        fail "could not make the amp-inbox agent token; if one exists, revoke it with: cpctl token rm amp-inbox"
    fi
    # The docs of this release: a branch's page can reach an agent from a
    # stale cache, and a tag's cannot change.
    docs=$repo/blob/$version/docs/amp.md
    [ "$version" != local ] || docs=$repo/blob/master/docs/amp.md
    say ""
    say "In Amp, create a new thread and paste this in:"
    say ""
    say "     Add the project ${repo#https://github.com/}, then follow steps 2-5"
    say "     of $docs. Prompt me for CP_URL and"
    say "     CP_TOKEN and save them to that project, not as personal secrets."
    say "     Start the inbox as a new thread in that project; this thread is"
    say "     not the inbox."
    say ""
    say "When it asks, enter these (CP_TOKEN is an agent token for Amp, not"
    say "your owner token):"
    say "     $(printf '%-9s %s' CP_URL "$url")"
    secret_line CP_TOKEN "$agent_file"
    say ""
    say "The inbox registers itself with your daemon; nothing else to do."
    say ""
    say "To approve peers, use cpctl from this machine:"
    use_here
else
    say ""
    say "Use it from this machine:"
    use_here
    if [ "$agent" = local ]; then
        say ""
        say "Then tell your agent: run cpctl help. It's the manual written for agents."
    fi
fi
share "$name" "$url"
say ""
say "Run this script again any time to update."
