#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/clanker-proxy-test.XXXXXXXX")
trap 'rm -rf "$work"' 0
trap 'exit 1' HUP INT TERM
mkdir -p "$work/tools" "$work/fixtures" "$work/source" "$work/tmp"

cat > "$work/tools/curl" <<'EOF'
#!/bin/sh
set -eu
output=
proto=
proto_redir=
for arg do
    printf '%s\n' "$arg" >> "$TEST_CASE/arguments"
done
while [ "$#" -gt 0 ]; do
    case $1 in
        --output) output=$2; shift 2 ;;
        --proto) proto=$2; shift 2 ;;
        --proto-redir) proto_redir=$2; shift 2 ;;
        --connect-timeout|--max-time|--retry|--max-filesize|--write-out) shift 2 ;;
        --disable|--fail|--silent|--show-error|--location|--head) shift ;;
        https://*) url=$1; shift ;;
        *) exit 90 ;;
    esac
done
[ "$proto" = '=https' ] && [ "$proto_redir" = '=https' ] || exit 91
printf '%s\n' "$url" >> "$TEST_CASE/requests"
case $url in
    https://github.com/Savid/clanker-proxy/releases/latest)
        printf '%s' "${TEST_REDIRECT:-https://github.com/Savid/clanker-proxy/releases/tag/v1.2.3}" ;;
    https://github.com/Savid/clanker-proxy/releases/download/v1.2.3/*)
        [ "${TEST_FAIL_DOWNLOAD:-}" != "${url##*/}" ] || exit 22
        cp "$TEST_FIXTURES/${url##*/}" "$output" ;;
    *) exit 92 ;;
esac
EOF
cat > "$work/tools/uname" <<'EOF'
#!/bin/sh
case $1 in
    -s) printf '%s\n' "${TEST_OS:-Linux}" ;;
    -m) printf '%s\n' "${TEST_ARCH:-x86_64}" ;;
    *) exit 1 ;;
esac
EOF
chmod +x "$work/tools/curl" "$work/tools/uname"
cat > "$work/tools/mv" <<'EOF'
#!/bin/sh
if [ "${TEST_FAIL_RENAME:-}" = cpctl ] && [ "$1" = -f ]; then
    case $2 in */.clanker-proxy.*/cpctl) exit 1 ;; esac
fi
exec /bin/mv "$@"
EOF
chmod +x "$work/tools/mv"
export PATH="$work/tools:$PATH"
export TEST_FIXTURES="$work/fixtures"
export TMPDIR="$work/tmp"
export TEST_CASE
export CP_INSTALL_DIR CP_VERSION

die() {
    printf 'FAIL: %s\n' "$*" >&2
    cat "$TEST_CASE/output" >&2
    exit 1
}

digest() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    else
        shasum -a 256 "$1" | awk '{ print $1 }'
    fi
}

fixture() {
    rm -f "$work/source/cpd" "$work/source/cpctl"
    printf '#!/bin/sh\nprintf "cpd fixture\\n"\n' > "$work/source/cpd"
    printf '#!/bin/sh\nprintf "cpctl fixture\\n"\n' > "$work/source/cpctl"
    tar -czf "$TEST_FIXTURES/clanker-proxy_linux_amd64.tar.gz" -C "$work/source" cpd cpctl
    checksums
}

checksums() {
    digest=$(digest "$TEST_FIXTURES/clanker-proxy_linux_amd64.tar.gz")
    printf '%s  clanker-proxy_linux_amd64.tar.gz\n' "$digest" > "$TEST_FIXTURES/checksums.txt"
}

start() {
    TEST_CASE=$work/$1
    mkdir -p "$TEST_CASE/bin"
    CP_INSTALL_DIR=$TEST_CASE/bin
    CP_VERSION=
    printf 'old cpd\n' > "$CP_INSTALL_DIR/cpd"
    printf 'old cpctl\n' > "$CP_INSTALL_DIR/cpctl"
    unset TEST_REDIRECT TEST_FAIL_DOWNLOAD TEST_OS TEST_ARCH TEST_FAIL_RENAME
    fixture
}

success() {
    sh "$root/scripts/install.sh" > "$TEST_CASE/output" 2>&1 || die 'installer failed'
    [ "$("$CP_INSTALL_DIR/cpd")" = 'cpd fixture' ] || die 'cpd was not installed'
    [ "$("$CP_INSTALL_DIR/cpctl")" = 'cpctl fixture' ] || die 'cpctl was not installed'
    cleaned
    printf 'ok: %s\n' "${TEST_CASE##*/}"
}

failure() {
    if sh "$root/scripts/install.sh" > "$TEST_CASE/output" 2>&1; then
        die 'installer unexpectedly succeeded'
    fi
    [ "$(cat "$CP_INSTALL_DIR/cpd")" = 'old cpd' ] || die 'failure replaced cpd'
    [ "$(cat "$CP_INSTALL_DIR/cpctl")" = 'old cpctl' ] || die 'failure replaced cpctl'
    cleaned
    printf 'ok: %s\n' "${TEST_CASE##*/}"
}

cleaned() {
    for entry in "$work/tmp"/* "$CP_INSTALL_DIR"/.clanker-proxy.*; do
        [ ! -e "$entry" ] || die "temporary file remains: $entry"
    done
    [ ! -d "$CP_INSTALL_DIR/.clanker-proxy-update.lock" ] || die 'update lock remains'
}

start latest
success
[ "$(wc -l < "$TEST_CASE/requests" | tr -d ' ')" = 3 ] || die 'latest must resolve once'

start pinned
CP_VERSION=v1.2.3
success
[ "$(wc -l < "$TEST_CASE/requests" | tr -d ' ')" = 2 ] || die 'pinned version must skip latest lookup'

start 'directory with spaces'
success

start macos-arm64
export TEST_OS=Darwin TEST_ARCH=arm64
cp "$TEST_FIXTURES/clanker-proxy_linux_amd64.tar.gz" "$TEST_FIXTURES/clanker-proxy_darwin_arm64.tar.gz"
sed 's/linux_amd64/darwin_arm64/' "$TEST_FIXTURES/checksums.txt" > "$work/checksums"
mv "$work/checksums" "$TEST_FIXTURES/checksums.txt"
success

start checksum-mismatch
printf 'corrupt' >> "$TEST_FIXTURES/clanker-proxy_linux_amd64.tar.gz"
failure

start missing-checksum
printf '%s  other.tar.gz\n' "$digest" > "$TEST_FIXTURES/checksums.txt"
failure

start duplicate-checksum
cat "$TEST_FIXTURES/checksums.txt" > "$work/checksums"
cat "$work/checksums" >> "$TEST_FIXTURES/checksums.txt"
failure

start missing-binary
tar -czf "$TEST_FIXTURES/clanker-proxy_linux_amd64.tar.gz" -C "$work/source" cpd
checksums
failure

start nested-binary-member
tar -cf "$work/nested.tar" -C "$work/source" cpd cpctl
rm "$work/source/cpctl"
mkdir "$work/source/cpctl"
printf 'unexpected bytes\n' > "$work/source/cpctl/extra"
tar -rf "$work/nested.tar" -C "$work/source" cpctl/extra
gzip -c "$work/nested.tar" > "$TEST_FIXTURES/clanker-proxy_linux_amd64.tar.gz"
rm -rf "$work/source/cpctl"
checksums
failure

start symlink-in-archive
rm "$work/source/cpctl"
ln -s /etc/passwd "$work/source/cpctl"
tar -czf "$TEST_FIXTURES/clanker-proxy_linux_amd64.tar.gz" -C "$work/source" cpd cpctl
checksums
failure

start empty-binary
: > "$work/source/cpctl"
tar -czf "$TEST_FIXTURES/clanker-proxy_linux_amd64.tar.gz" -C "$work/source" cpd cpctl
checksums
failure

start invalid-archive
printf 'not an archive\n' > "$TEST_FIXTURES/clanker-proxy_linux_amd64.tar.gz"
checksums
failure

start failed-download
export TEST_FAIL_DOWNLOAD=checksums.txt
failure

start invalid-version
CP_VERSION=v1.2.3/../other
failure
[ ! -e "$TEST_CASE/requests" ] || die 'invalid pin made a network request'

start leading-zero-version
CP_VERSION=v01.2.3
failure

start multiline-version
CP_VERSION='v1.2.3
v4.5.6'
failure
[ ! -e "$TEST_CASE/requests" ] || die 'multiline pin made a network request'

start bad-latest-redirect
export TEST_REDIRECT=https://example.com/releases/tag/v1.2.3
failure

start unsupported-platform
export TEST_OS=FreeBSD
failure

start existing-symlink
mv "$CP_INSTALL_DIR/cpctl" "$TEST_CASE/managed-cpctl"
ln -s "$TEST_CASE/managed-cpctl" "$CP_INSTALL_DIR/cpctl"
failure
[ -L "$CP_INSTALL_DIR/cpctl" ] || die 'existing symlink was replaced'

start second-rename-failed
export TEST_FAIL_RENAME=cpctl
failure

start concurrent-update
mkdir "$CP_INSTALL_DIR/.clanker-proxy-update.lock"
if sh "$root/scripts/install.sh" > "$TEST_CASE/output" 2>&1; then
    die 'installer ignored update lock'
fi
[ -d "$CP_INSTALL_DIR/.clanker-proxy-update.lock" ] || die 'installer removed another update lock'
[ "$(cat "$CP_INSTALL_DIR/cpd")" = 'old cpd' ] || die 'locked installer changed cpd'
[ "$(cat "$CP_INSTALL_DIR/cpctl")" = 'old cpctl' ] || die 'locked installer changed cpctl'
rmdir "$CP_INSTALL_DIR/.clanker-proxy-update.lock"
cleaned
printf 'ok: concurrent-update\n'

printf 'All installer tests passed.\n'
