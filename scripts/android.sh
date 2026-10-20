#!/bin/sh
# Build the Android app. Tauri's Android build uses symlinks, which some
# network shares (SMB/CIFS) do not support, so the source is mirrored to local
# disk first and the APK is copied back to ./out/.
#
#   sh scripts/android.sh [debug|release] [x86_64|aarch64|all]
set -eu
MODE="${1:-debug}"
ARCH="${2:-aarch64}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TOOLCHAIN="${TESSERA_TOOLCHAIN:-$HOME/.local/share/tessera-toolchain}"
# shellcheck disable=SC1091
[ -f "$TOOLCHAIN/env.sh" ] && . "$TOOLCHAIN/env.sh"
BUILD="${TESSERA_ANDROID_BUILD:-$HOME/.cache/tessera/android-build}"
# Public builds must use an existing, private release key, never the local fallback.
if [ "${TESSERA_ANDROID_PUBLIC_RELEASE:-0}" = 1 ]; then
  [ "$MODE" = release ] || { echo 'ABORT: public APKs must use release mode.' >&2; exit 1; }
  if [ -z "${TESSERA_KEYSTORE:-}" ] || [ ! -f "$TESSERA_KEYSTORE" ] \
    || [ -z "${TESSERA_KEYSTORE_PASS_FILE:-}" ] || [ ! -s "$TESSERA_KEYSTORE_PASS_FILE" ] \
    || [ -z "${TESSERA_KEY_ALIAS:-}" ]; then
    echo 'ABORT: public release needs TESSERA_KEYSTORE, TESSERA_KEYSTORE_PASS_FILE and TESSERA_KEY_ALIAS. No key will be generated.' >&2
    exit 1
  fi
fi
case "$MODE:$ARCH" in debug:aarch64|debug:x86_64|debug:all|release:aarch64|release:x86_64|release:all) ;; *) echo 'Invalid Android mode or architecture.' >&2; exit 1 ;; esac

# Tauri writes these once (`cargo tauri android init`) and git ignores them, so a copy of the
# source from git lacks them. Without proguard-tauri.pro the release shrinker drops the methods
# the Rust side calls (getPluginManager) and the app crashes the moment it opens.
for f in gen/android/app/proguard-tauri.pro gen/android/app/tauri.build.gradle.kts gen/android/tauri.settings.gradle; do
  if [ ! -f "$ROOT/apps/tessera/src-tauri/$f" ]; then
    echo "ABORT: apps/tessera/src-tauri/$f is missing (made by 'cargo tauri android init', not in git); the APK would crash on start." >&2
    exit 1
  fi
done

# SECURITY: the Android WebView must not inject Tauri's IPC bridge into iframes
# (whiteboards show untrusted content there). The vendored wry scopes injection
# to the app origin (see vendor/README.md); refuse to build without it.
ALL_FRAMES='addDocumentStartJavaScript(this, script, setOf("\*"))'
if ! grep -q 'wry = { path = "vendor/wry" }' "$ROOT/Cargo.toml" \
  || grep -q "$ALL_FRAMES" "$ROOT/vendor/wry/src/android/kotlin/RustWebView.kt" \
  || ! grep -q '!is_document_start_script_enabled && own_pages' "$ROOT/vendor/wry/src/android/mod.rs"; then
  echo "SECURITY ABORT: the patched wry (vendor/wry) is missing or reverted; see vendor/README.md." >&2
  exit 1
fi

echo "==> building web UI"
# npm ci: exactly the versions in package-lock.json, and no package's install scripts run.
( cd "$ROOT/apps/tessera/ui" && { [ -d node_modules ] || npm ci --no-bin-links --ignore-scripts --no-audit --no-fund; } ) >/dev/null
( cd "$ROOT/apps/tessera/ui" && npm run build )
# The licence texts travel inside the interface (lib/licences.ts), so inside the APK: an app
# without them may not be handed out.
if ! grep -lq 'THIRD_PARTY_NOTICES.md explains what is where' "$ROOT/apps/tessera/ui/dist/assets/"*.js; then
  echo "ABORT: the web UI build lacks the licence texts (apps/tessera/ui/src/lib/licences.ts)." >&2
  exit 1
fi

echo "==> mirroring sources to $BUILD"
mkdir -p "$BUILD"
rsync -a --delete \
  --exclude '/target' --exclude '/out' --exclude 'node_modules' --exclude '.git' \
  --exclude 'gen/android/app/build' --exclude 'gen/android/.gradle' --exclude 'gen/android/buildSrc/build' \
  --exclude 'gen/android/app/src/main/jniLibs' \
  --exclude 'gen/android/app/src/main/java/dev/tessera/app/generated' \
  "$ROOT/" "$BUILD/src/"
# wry writes its Kotlin (with Tessera's patches, vendor/wry) into `generated/`
# when its build script runs. Those files belong to the mirror: a copy from the
# repo would carry an old timestamp, cargo would see nothing newer and Gradle
# would compile stale Kotlin without the patches. When the patched Kotlin
# changes, drop the old output so wry writes it again.
GEN_DIR="$BUILD/src/apps/tessera/src-tauri/gen/android/app/src/main/java/dev/tessera/app/generated"
KOTLIN_SUM=$(cat "$ROOT"/vendor/wry/src/android/kotlin/*.kt | sha256sum | cut -d' ' -f1)
if [ "$(cat "$BUILD/wry-kotlin.sha256" 2>/dev/null)" != "$KOTLIN_SUM" ]; then
  rm -rf "$GEN_DIR"
  echo "$KOTLIN_SUM" > "$BUILD/wry-kotlin.sha256"
fi
chmod +x "$BUILD/src/apps/tessera/src-tauri/gen/android/gradlew" 2>/dev/null || true
# Re-run tauri-build so plugin Android libraries are always wired into Gradle,
# and our own plugin's build script too: it records the path of its Android
# sources, which must be this mirror's (the target dir is shared).
touch "$BUILD/src/apps/tessera/src-tauri/build.rs" "$BUILD/src/apps/tessera/src-tauri/plugins/background/build.rs"

set -- --apk
[ "$MODE" = "debug" ] && set -- "$@" --debug
if [ "$ARCH" = "all" ]; then set -- "$@" --target aarch64 --target armv7 --target x86_64; else set -- "$@" --target "$ARCH"; fi

# Do not re-sign or copy a stale APK from an earlier build.
rm -rf "$BUILD/src/apps/tessera/src-tauri/gen/android/app/build/outputs/apk"
echo "==> cargo tauri android build $*"
( cd "$BUILD/src/apps/tessera/src-tauri" && CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/tessera/target}" cargo tauri android build "$@" )

# SECURITY: check what was actually compiled into this APK.
GEN_WEBVIEW="$BUILD/src/apps/tessera/src-tauri/gen/android/app/src/main/java/dev/tessera/app/generated/RustWebView.kt"
if ! grep -q "appOrigins" "$GEN_WEBVIEW" || grep -q "$ALL_FRAMES" "$GEN_WEBVIEW"; then
  echo "SECURITY ABORT: the built WebView injects Tauri's IPC bridge into all frames; not shipping this APK." >&2
  exit 1
fi
# Every Tessera patch in vendor/wry's Kotlin must be in what Gradle compiled.
for f in "$ROOT"/vendor/wry/src/android/kotlin/*.kt; do
  # (grep -c exits 1 when it counts none, which `set -e` would take for a failure.)
  want=$(grep -c "TESSERA" "$f" || true)
  have=$(grep -c "TESSERA" "$GEN_DIR/$(basename "$f")" 2>/dev/null || true); have=${have:-0}
  if [ "$want" != "$have" ]; then
    echo "ABORT: $(basename "$f") was built without Tessera's Android patches ($have of $want); not shipping this APK." >&2
    rm -f "$BUILD/wry-kotlin.sha256"
    exit 1
  fi
done

mkdir -p "$ROOT/out"
# Release APKs come out unsigned. Sign them for sideloading with a local key
# ($TESSERA_KEYSTORE, default: a debug key kept with the toolchain).
if [ "$MODE" = "release" ]; then
  KS="${TESSERA_KEYSTORE:-$TOOLCHAIN/android-user/tessera-sideload.jks}"
  # The key's password: read from TESSERA_KEYSTORE_PASS_FILE (keep it where only
  # you can read it, outside any folder agents may use on this computer), never
  # on a command line. Without it, the fixed password keys made before use.
  if [ -n "${TESSERA_KEYSTORE_PASS_FILE:-}" ]; then
    KS_PASS="file:$TESSERA_KEYSTORE_PASS_FILE"
    KT_PASS="-storepass:file $TESSERA_KEYSTORE_PASS_FILE -keypass:file $TESSERA_KEYSTORE_PASS_FILE"
  else
    KS_PASS="pass:tessera"
    KT_PASS="-storepass tessera -keypass tessera"
  fi
  if [ ! -f "$KS" ]; then
    # shellcheck disable=SC2086
    (umask 077 && keytool -genkeypair -keystore "$KS" $KT_PASS -alias tessera \
      -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Tessera sideload" >/dev/null)
  fi
  # Whoever reads the key (and its password) can sign updates the phone accepts.
  chmod 600 "$KS"
  BT="$(find "$ANDROID_HOME/build-tools" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -1)"
  APK_DIR="$BUILD/src/apps/tessera/src-tauri/gen/android/app/build/outputs/apk"
  [ -n "$(find "$APK_DIR" -path '*/release/*' -name '*-unsigned.apk' -print -quit)" ] || {
    echo 'ABORT: no unsigned release APK was produced.' >&2; exit 1
  }
  find "$APK_DIR" -path '*/release/*' -name '*-unsigned.apk' | while read -r apk; do
    out="${apk%-unsigned.apk}.apk"
    "$BT/zipalign" -f -p 4 "$apk" "$out.aligned"
    "$BT/apksigner" sign --ks "$KS" --ks-key-alias "${TESSERA_KEY_ALIAS:-tessera}" --ks-pass "$KS_PASS" --key-pass "$KS_PASS" --out "$out" "$out.aligned"
    "$BT/apksigner" verify "$out"
    rm -f "$out.aligned" "$apk"
  done
fi
find "$BUILD/src/apps/tessera/src-tauri/gen/android/app/build/outputs/apk" -path "*/$MODE/*" -name '*.apk' | while read -r apk; do
  name="tessera-$(basename "$(dirname "$(dirname "$apk")")")-$(basename "$(dirname "$apk")")-$ARCH.apk"
  cp "$apk" "$ROOT/out/$name"
  echo "==> $ROOT/out/$name"
done
