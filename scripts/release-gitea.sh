#!/bin/sh
# Build a complete Linux, Windows and Android launch release on Gitea/Forgejo.
# Linux/Windows programs are built here. Supply the signed ARM64 APK built
# from this same source with scripts/android.sh in public release mode.
#
#   sh scripts/release-gitea.sh v0.1.0 --repo https://host/owner/tessera
#   sh scripts/release-gitea.sh v0.1.0 --repo https://host/owner/tessera --dry-run
#
# Options:
#   --repo URL       the project's page on the server
#                    (or set $TESSERA_RELEASE_REPO; no publishing target is assumed)
#   --notes TEXT     the release's description (default: a short one)
#   --prerelease     mark it as a pre-release
#   --skip-build     package complete existing target outputs (still checked)
#   --android-apk P  signed public ARM64 APK (required)
#   --dry-run        don't contact the server
#
# Publishing always requires both Linux CLIs with UI, the Linux desktop,
# the Windows zip and complete NSIS installer, the signed Android APK,
# both install scripts and SHA256SUMS. No partial-release switch.
#
# Needs rustup targets x86_64/aarch64-unknown-linux-gnu and
# x86_64-pc-windows-msvc, cargo-xwin, clang-cl, lld-link, llvm-lib, zig,
# cargo-zigbuild, tauri-cli (version in install.sh), NSIS, Linux WebKitGTK
# 4.1 development libraries, Node 22, Python 3, 7z and Android SDK
# build-tools (apksigner and aapt on PATH). Microsoft's SDK/CRT licence
# applies to cargo-xwin downloads. Set TESSERA_BUILD_WRAP to run builds
# through e.g. `flock -o /tmp/tessera-build.lock nice -n 19 ionice -c3`.
#
# The server login comes from git's saved credentials for that server
# (`git credential fill`); it is never printed or put on a command line.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
REPO="${TESSERA_RELEASE_REPO:-}"
NOTES=""
PRE=false
SKIP_BUILD=""
DRY=""
ANDROID_APK=""
TAG=""
WRAP="${TESSERA_BUILD_WRAP:-}"
WIN_TARGET=x86_64-pc-windows-msvc
# The C runtime linked in: the programs then run on a Windows without the
# Visual C++ redistributable (vcruntime140.dll).
export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="${CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS:--C target-feature=+crt-static}"
LINUX_TARGET=x86_64-unknown-linux-gnu
GLIBC=2.28

say() { printf '  %s\n' "$*"; }
fail() { printf '  error: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --repo) [ $# -gt 1 ] || fail "give the project URL after --repo"; REPO=$2; shift ;;
    --notes) [ $# -gt 1 ] || fail "give the release text after --notes"; NOTES=$2; shift ;;
    --prerelease) PRE=true ;;
    --skip-build) SKIP_BUILD=1 ;;
    --android-apk) [ $# -gt 1 ] || fail 'give a signed APK after --android-apk'; ANDROID_APK=$2; shift ;;
    --dry-run) DRY=1 ;;
    -h | --help) sed -n '2,32p' "$0"; exit 0 ;;
    v*) TAG=$1 ;;
    *) fail "unknown option $1 (try --help)" ;;
  esac
  shift
done
[ -n "$TAG" ] || fail "give the release's tag, e.g. v0.1.0"
[ -n "$REPO" ] || fail "choose the Gitea or Forgejo project with --repo https://host/owner/name (or TESSERA_RELEASE_REPO)"
REPO=${REPO%/}
VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml")
if [ "v$VERSION" != "${TAG%%-*}" ]; then
  fail "the tag $TAG doesn't match version $VERSION in Cargo.toml"
fi

# owner/name and the server behind the project's page.
SERVER=$(printf '%s' "$REPO" | sed -n 's|^\(https\{0,1\}://[^/]*\)/.*|\1|p')
OWNER_NAME=$(printf '%s' "$REPO" | sed -n 's|^https\{0,1\}://[^/]*/\([^/]*/[^/]*\)$|\1|p')
if [ -z "$SERVER" ] || [ -z "$OWNER_NAME" ]; then fail "$REPO isn't a project's page (http://host/owner/name)"; fi
API="$SERVER/api/v1/repos/$OWNER_NAME"
node "$ROOT/scripts/release-links.mjs" --check || fail "project links are out of date; run node scripts/release-links.mjs --write"
if [ -z "$ANDROID_APK" ] || [ ! -f "$ANDROID_APK" ]; then fail 'supply --android-apk PATH built from this source in public release mode (see docs/RELEASING.md)'; fi
ANDROID_APK=$(realpath "$ANDROID_APK")

TARGET=$(cd "$ROOT" && cargo metadata --format-version 1 --no-deps 2>/dev/null | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
[ -n "$TARGET" ] || TARGET="$ROOT/target"
OUT="$ROOT/out/release"
rm -rf "$OUT"
mkdir -p "$OUT"

# ── Build ─────────────────────────────────────────────────────────────

if [ -z "$SKIP_BUILD" ]; then
  # The web app for "Use Tessera in a web browser" goes into the server.
  say "Building the web app"
  (cd "$ROOT/apps/tessera/ui" && { [ -d node_modules ] || npm ci --no-audit --no-fund; } && npm run build)
  [ -f "$ROOT/apps/tessera/ui/dist/index.html" ] || fail "the web app didn't build"
  export TESSERA_WEB_APP="$ROOT/apps/tessera/ui/dist"
  say "Building for Windows ($WIN_TARGET)"
  (cd "$ROOT" && $WRAP cargo xwin build --release --locked --target "$WIN_TARGET" -p tessera -p tessera-service)
  say "Building for Linux ($LINUX_TARGET, glibc $GLIBC and newer)"
  (cd "$ROOT" && $WRAP cargo zigbuild --release --locked -p tessera --target "$LINUX_TARGET.$GLIBC")
  say 'Building for Linux ARM64'
  (cd "$ROOT" && $WRAP cargo zigbuild --release --locked -p tessera --target "aarch64-unknown-linux-gnu.$GLIBC")
  say 'Building the Linux desktop'
  (cd "$ROOT/apps/tessera/src-tauri" && $WRAP cargo tauri build --no-bundle -- --locked)
  say 'Building the complete Windows installer'
  mkdir -p "$ROOT/apps/tessera/src-tauri/windows/resources"
  cp "$TARGET/$WIN_TARGET/release/tessera.exe" "$TARGET/$WIN_TARGET/release/tessera-service.exe" "$ROOT/apps/tessera/src-tauri/windows/resources/"
  (cd "$ROOT/apps/tessera/src-tauri" && $WRAP cargo tauri build --runner cargo-xwin --target "$WIN_TARGET" --bundles nsis --config windows/installer.conf.json -- --locked)
fi

# ── Package ───────────────────────────────────────────────────────────

# The installers, pointed at this server.
sed "s|^\$TesseraRepo = '.*'|\$TesseraRepo = '$REPO'|" "$ROOT/scripts/install.ps1" > "$OUT/install.ps1"
sed "s|^REPO_URL=\"\${TESSERA_REPO_URL:-.*}\"|REPO_URL=\"\${TESSERA_REPO_URL:-$REPO}\"|" "$ROOT/scripts/install.sh" > "$OUT/install.sh"
grep -q "^\$TesseraRepo = '$REPO'" "$OUT/install.ps1" || fail "couldn't point install.ps1 at $REPO"
grep -q "TESSERA_REPO_URL:-$REPO}" "$OUT/install.sh" || fail "couldn't point install.sh at $REPO"

win="$TARGET/$WIN_TARGET/release"
for f in tessera.exe tessera-service.exe; do
  [ -f "$win/$f" ] || fail "$win/$f is missing (build it, or leave out --skip-build)"
done
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/win" "$stage/linux"
cp "$win/tessera.exe" "$win/tessera-service.exe" "$OUT/install.ps1" "$stage/win/"
# The licence and the third-party notices travel with the programs.
for f in LICENSE NOTICE THIRD_PARTY_NOTICES.md THIRD_PARTY_LICENSES.txt; do
  if [ -f "$ROOT/$f" ]; then cp "$ROOT/$f" "$stage/win/"; cp "$ROOT/$f" "$stage/linux/"; fi
done
# zip is not everywhere; Python is.
(cd "$stage/win" && python3 -c '
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w", zipfile.ZIP_DEFLATED) as z:
    for name in sys.argv[2:]:
        z.write(name)
' "$OUT/tessera-windows-x86_64.zip" ./*)

for arch in x86_64 aarch64; do
  linux="$TARGET/$arch-unknown-linux-gnu/release/tessera"
  [ -f "$linux" ] || fail "$linux is missing (build it, or leave out --skip-build)"
  cp "$linux" "$stage/linux/tessera"
  (cd "$stage/linux" && tar -czf "$OUT/tessera-linux-$arch.tar.gz" -- *)
done
mkdir -p "$stage/desktop/icons"
cp "$TARGET/release/tessera-app" "$stage/desktop/tessera-desktop"
cp "$ROOT/apps/tessera/src-tauri/icons/128x128@2x.png" "$stage/desktop/icons/256x256.png"
cp "$ROOT/apps/tessera/src-tauri/icons/32x32.png" "$stage/desktop/icons/32x32.png"
for f in LICENSE NOTICE THIRD_PARTY_NOTICES.md THIRD_PARTY_LICENSES.txt; do
  if [ -f "$ROOT/$f" ]; then cp "$ROOT/$f" "$stage/desktop/"; fi
done
(cd "$stage/desktop" && tar -czf "$OUT/tessera-desktop-linux-x86_64.tar.gz" -- *)
set -- "$win"/bundle/nsis/*-setup.exe
if [ "$#" != 1 ] || [ ! -f "$1" ]; then fail 'expected exactly one built Windows NSIS installer (clean stale bundle output)'; fi
cp "$1" "$OUT/tessera-desktop-windows-x86_64-setup.exe"
cp "$ANDROID_APK" "$OUT/tessera-android-arm64.apk"

(cd "$OUT" && sha256sum -- * > SHA256SUMS)
python3 "$ROOT/scripts/check-release.py" "$OUT" || fail 'release is incomplete; nothing was published'
say "Packaged in $OUT:"
sed 's/^/    /' "$OUT/SHA256SUMS"

if [ -n "$DRY" ]; then
  say "Dry run: nothing was published."
  exit 0
fi

# ── Publish ───────────────────────────────────────────────────────────

# git's saved login for this server, into a private file curl reads, so it
# never shows on a command line or in the output.
host=${SERVER#*://}
proto=${SERVER%%://*}
cred=$(printf 'protocol=%s\nhost=%s\n\n' "$proto" "$host" | GIT_TERMINAL_PROMPT=0 git credential fill 2>/dev/null) ||
  fail "no saved git login for $SERVER (sign in once with git, e.g. by cloning over HTTP)"
user=$(printf '%s\n' "$cred" | sed -n 's/^username=//p')
pass=$(printf '%s\n' "$cred" | sed -n 's/^password=//p')
if [ -z "$user" ] || [ -z "$pass" ]; then fail "the saved git login for $SERVER is incomplete"; fi
netrc=$(mktemp)
chmod 600 "$netrc"
trap 'rm -rf "$stage" "$netrc"' EXIT
host_only=${host%%:*}
printf 'machine %s\nlogin %s\npassword %s\n' "$host_only" "$user" "$pass" > "$netrc"
unset cred user pass

api() { curl -fsS --netrc-file "$netrc" "$@"; }

commit=$(cd "$ROOT" && git rev-parse HEAD)
[ -n "$NOTES" ] || NOTES="Tessera $TAG. Windows: run install.ps1 (see docs/WINDOWS.md). Linux: install.sh."
existing=$(api "$API/releases/tags/$TAG" 2>/dev/null || true)
id=$(printf '%s' "$existing" | python3 -c 'import json,sys
try: print(json.load(sys.stdin)["id"])
except Exception: pass')
if [ -z "$id" ]; then
  body=$(python3 -c 'import json,sys; print(json.dumps({"tag_name": sys.argv[1], "target_commitish": sys.argv[2], "name": "Tessera " + sys.argv[1], "body": sys.argv[3], "prerelease": sys.argv[4] == "true"}))' "$TAG" "$commit" "$NOTES" "$PRE")
  id=$(api -X POST -H 'Content-Type: application/json' -d "$body" "$API/releases" |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])') || fail "couldn't create the release $TAG"
  say "Created the release $TAG ($REPO/releases/tag/$TAG)"
else
  say "The release $TAG exists: replacing its files"
fi
# Files already there go first (same names).
old=$(api "$API/releases/$id/assets" | python3 -c 'import json,sys
for a in json.load(sys.stdin): print(a["id"], a["name"])')
for f in "$OUT"/*; do
  name=$(basename "$f")
  printf '%s\n' "$old" | while read -r aid aname; do
    if [ "$aname" = "$name" ]; then api -X DELETE "$API/releases/$id/assets/$aid" > /dev/null; fi
  done
  api -X POST -F "attachment=@$f" "$API/releases/$id/assets?name=$name" > /dev/null || fail "uploading $name failed"
  say "Uploaded $name"
done
say "Done: $REPO/releases/tag/$TAG"
