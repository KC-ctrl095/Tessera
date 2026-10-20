#!/bin/sh
# Install or update Tessera for the current user.
#
#   curl -fsSL https://github.com/KC-ctrl095/Tessera/releases/latest/download/install.sh | sh
#   sh scripts/install.sh            in a copy of the source: builds that copy
#
# Options:
#   --yes, -y        answer yes to every question
#   --no-desktop     leave out the desktop app
#   --source         build from source even when a ready-made build exists
#   --version TAG    install that release (e.g. v0.2.0) instead of the latest
#   --help           show this
#
# What goes where (nothing needs sudo; it asks before installing a missing
# system package):
#   ~/.local/bin/tessera            the server, the terminal app and `tessera setup`
#   ~/.local/bin/tessera-desktop    the desktop app (Linux with WebKitGTK)
#   an app-menu entry and icon      for the desktop app
#   a browser for web views         Chrome for Testing, only when none is installed
# A ready-made build is checked against the release's SHA256SUMS before
# anything is installed. Without one for this computer, it builds from source
# (Rust through rustup, no sudo). Then it offers `tessera setup`.
#
# Every question defaults to yes (press Enter). Without a terminal (scripts,
# CI) the answers are yes and setup isn't started. What the tools print goes
# to a log, shown when something fails.
#
# Everything is in functions and `main` runs last, so a download cut short
# can't run half a script.
set -eu

# Where Tessera lives. The code reads it from `repository` in Cargo.toml;
# this is the standalone installer's generated copy. After changing it in
# Cargo.toml, run node scripts/release-links.mjs --write. A fork or mirror can
# override it with TESSERA_REPO_URL.
REPO_URL="${TESSERA_REPO_URL:-https://github.com/KC-ctrl095/Tessera}"

BIN="${TESSERA_BIN_DIR:-$HOME/.local/bin}"
TOOLCHAIN="${TESSERA_TOOLCHAIN:-$HOME/.local/share/tessera-toolchain}"
# Tessera looks here for a browser when none is installed (see browser/process.rs).
BROWSER_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/tessera-browser"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/tessera"
LOG="$CACHE/install.log"
# Cargo.toml's rust-version (1.88).
RUST_MIN=88
# The desktop app's build tool, the version that matches Cargo.lock's tauri
# (an exact one: what builds it never changes underneath).
TAURI_CLI=2.12.1
# The ready-made Linux builds run on glibc 2.28 and newer (see
# .github/workflows/release.yml); older systems build from source.
GLIBC_MIN=28
NO_DESKTOP=""
YES=""
FROM_SOURCE=""
TAG="${TESSERA_VERSION:-}"

usage() {
  cat <<'USAGE'
Install or update Tessera for the current user.

  curl -fsSL <release>/install.sh | sh                 a ready-made build (or from source)
  curl -fsSL <release>/install.sh | sh -s -- OPTIONS   the same, with options
  sh scripts/install.sh [OPTIONS]                      in a copy of the source: builds it

Options:
  --yes, -y        answer yes to every question
  --no-desktop     leave out the desktop app
  --source         build from source even when a ready-made build exists
  --version TAG    install that release (e.g. v0.2.0) instead of the latest
  --help           show this

It installs into ~/.local/bin without sudo, asks before installing a missing
system package, and then offers `tessera setup`. The log of the last run is
in ~/.cache/tessera/install.log.
USAGE
}

# ── Output ────────────────────────────────────────────────────────────

setup_output() {
  TTY=""
  if [ -t 1 ]; then TTY=1; fi
  case "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}" in
    *[Uu][Tt][Ff]-8* | *[Uu][Tt][Ff]8*) UTF=1 ;;
    *) UTF="" ;;
  esac
  GOLD="" OK="" WARN="" DIM="" B="" R=""
  if [ -n "$TTY" ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-dumb}" != dumb ]; then
    case "${COLORTERM:-}${TERM:-}" in
      *truecolor* | *24bit* | *256*)
        GOLD='\033[38;5;179m' OK='\033[38;5;72m' WARN='\033[38;5;173m' DIM='\033[38;5;245m' ;;
      *) GOLD='\033[33m' OK='\033[32m' WARN='\033[31m' DIM='\033[90m' ;;
    esac
    B='\033[1m' R='\033[0m'
  fi
  if [ -n "$UTF" ]; then
    G_OK='✓' G_SKIP='–' G_ERR='✗' G_ASK='◆'
  else
    G_OK='ok' G_SKIP='-' G_ERR='x' G_ASK='?'
  fi
  # Where a person can answer: this terminal, or (piped in from curl) the
  # terminal the pipe runs in. Empty: nobody to ask.
  ANSWER_FROM=""
  if [ -t 0 ]; then
    ANSWER_FROM=/dev/stdin
  elif [ -p /dev/stdin ] && [ -n "$TTY" ] && (exec < /dev/tty) 2>/dev/null; then
    ANSWER_FROM=/dev/tty
  fi
}

# ok TEXT [DETAIL] · skip TEXT · warn TEXT · note TEXT (indented, faint)
ok() {
  printf "  ${OK}%s${R} %s" "$G_OK" "$1"
  if [ $# -gt 1 ]; then printf "  ${DIM}%s${R}" "$2"; fi
  printf '\n'
}
skip() { printf "  ${DIM}%s %s${R}\n" "$G_SKIP" "$1"; }
warn() { printf "  ${WARN}! %s${R}\n" "$1"; }
note() { printf "    ${DIM}%s${R}\n" "$1"; }
# fail TEXT: stop, showing the end of the log.
fail() {
  printf "  ${WARN}%s %s${R}\n" "$G_ERR" "$1" >&2
  if [ -s "$LOG" ]; then
    printf '\n' >&2
    # A compiler's first error says the most; otherwise the end of the log.
    first=$(grep -n '^error' "$LOG" | head -n 1 | cut -d: -f1)
    if [ -n "$first" ]; then
      sed -n "${first},$((first + 19))p" "$LOG" | sed 's/^/    /' >&2
    else
      tail -n 20 "$LOG" | sed 's/^/    /' >&2
    fi
    printf "\n    ${DIM}The whole log: %s${R}\n" "$LOG" >&2
  fi
  exit 1
}

# ask QUESTION: yes/no; Enter (or --yes, or nobody to ask) means yes.
ask() {
  if [ -n "$YES" ] || [ -z "$ANSWER_FROM" ]; then return 0; fi
  printf "  ${GOLD}%s${R} %s ${DIM}[Y/n]${R} " "$G_ASK" "$1"
  read -r answer < "$ANSWER_FROM" || answer=""
  case "$answer" in [nN]*) return 1 ;; *) return 0 ;; esac
}

# ask_no QUESTION: yes/no; Enter (and --yes, and nobody to ask) means no.
ask_no() {
  if [ -n "$YES" ] || [ -z "$ANSWER_FROM" ]; then return 1; fi
  printf "  ${GOLD}%s${R} %s ${DIM}[y/N]${R} " "$G_ASK" "$1"
  read -r answer < "$ANSWER_FROM" || answer=""
  case "$answer" in [yY]*) return 0 ;; *) return 1 ;; esac
}

# 75s → "1m 15s"
dur() {
  if [ "$1" -ge 60 ]; then
    printf '%dm %02ds' $(($1 / 60)) $(($1 % 60))
  else
    printf '%ds' "$1"
  fi
}

JOB=""
TMP=""
cleanup() {
  if [ -n "$JOB" ]; then kill "$JOB" 2>/dev/null || true; fi
  if [ -n "$TMP" ]; then rm -rf "$TMP"; fi
  if [ -n "${TTY:-}" ]; then printf '\r\033[K\033[?25h'; fi
}

# run LABEL CMD...: run CMD with its output going to the log while LABEL
# shows with a spinner and the time so far; $DETAIL names a function that
# prints a few words of progress. Returns CMD's status; sets $TOOK.
DETAIL=""
run() {
  label=$1
  shift
  printf '\n## %s: %s\n' "$label" "$*" >> "$LOG"
  start=$(date +%s)
  if [ -z "$TTY" ]; then
    printf '  … %s\n' "$label"
    if "$@" >> "$LOG" 2>&1 < /dev/null; then st=0; else st=$?; fi
    TOOK=$(dur $(($(date +%s) - start)))
    return $st
  fi
  LOG_FROM=$(wc -l < "$LOG")
  "$@" >> "$LOG" 2>&1 < /dev/null &
  JOB=$!
  cols=$(stty size 2>/dev/null < /dev/tty | cut -d' ' -f2)
  cols=${cols:-80}
  n=0
  printf '\033[?25l'
  while kill -0 "$JOB" 2>/dev/null; do
    if [ -n "$UTF" ]; then
      case $((n % 4)) in 0) f='▖' ;; 1) f='▘' ;; 2) f='▝' ;; *) f='▗' ;; esac
    else
      case $((n % 4)) in 0) f='|' ;; 1) f='/' ;; 2) f='-' ;; *) f='\' ;; esac
    fi
    el=$(dur $(($(date +%s) - start)))
    d=""
    if [ -n "$DETAIL" ]; then d=$($DETAIL); fi
    room=$((cols - ${#label} - ${#el} - 12))
    if [ "$room" -lt 4 ]; then d=""; fi
    d=$(printf '%s' "$d" | cut -c "1-$((room > 1 ? room : 1))")
    printf "\r\033[K  ${GOLD}%s${R} %s  ${DIM}%s  %s${R}" "$f" "$label" "$el" "$d"
    n=$((n + 1))
    sleep 0.2 2>/dev/null || sleep 1
  done
  if wait "$JOB"; then st=0; else st=$?; fi
  JOB=""
  printf '\r\033[K\033[?25h'
  TOOK=$(dur $(($(date +%s) - start)))
  return $st
}

# Progress words for cargo: crates compiled in this step, and the latest.
cargo_detail() {
  tail -n "+$((LOG_FROM + 1))" "$LOG" | grep '^ *Compiling ' > "$LOG.part" || true
  c=$(wc -l < "$LOG.part")
  last=$(tail -n 1 "$LOG.part" | sed 's/^ *Compiling \([^ ]*\).*/\1/')
  rm -f "$LOG.part"
  if [ -n "$last" ]; then
    printf '%s crates · %s' "$c" "$last"
  elif tail -n 3 "$LOG" | grep -q -e Download -e Updating; then
    printf 'downloading crates'
  fi
}

# Progress words for a download: the size of $DL_FILE so far.
DL_FILE=""
download_detail() {
  if [ -f "$DL_FILE" ]; then
    printf '%s MB' $(($(wc -c < "$DL_FILE") / 1048576))
  fi
}

need_curl() {
  if ! command -v curl >/dev/null 2>&1; then
    fail "Downloading needs curl. On Debian or Ubuntu: sudo apt install curl"
  fi
}

# fetch URL FILE: HTTPS only (plain http just for a local test server).
fetch() {
  case "$1" in
    http://127.0.0.1[:/]* | http://localhost[:/]*) p='=http,https' ;;
    *) p='=https' ;;
  esac
  curl --proto "$p" --proto-redir "$p" --tlsv1.2 -fsSL --retry 2 --connect-timeout 15 -o "$2" "$1"
}

SUDO=""
if [ "$(id -u)" -ne 0 ]; then SUDO=sudo; fi
# Ask for the password up front, so the install itself can run quietly.
sudo_ready() {
  if [ -z "$SUDO" ]; then return 0; fi
  if ! command -v sudo >/dev/null 2>&1; then return 1; fi
  sudo -v
}

# pkg_install LABEL apt-names dnf-names pacman-names zypper-names: ask, then
# install with the system's package manager. Fails when it can't.
pkg_install() {
  what=$1
  if command -v apt-get >/dev/null 2>&1; then
    set -- sh -c "$SUDO apt-get update -qq && $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $2"
  elif command -v dnf >/dev/null 2>&1; then
    set -- sh -c "$SUDO dnf install -y -q $3"
  elif command -v pacman >/dev/null 2>&1; then
    set -- sh -c "$SUDO pacman -S --needed --noconfirm $4"
  elif command -v zypper >/dev/null 2>&1; then
    set -- sh -c "$SUDO zypper --quiet install -y $5"
  else
    return 1
  fi
  sudo_ready || return 1
  run "Installing $what" "$@"
}

# put_bin FILE DEST: install an executable, also over one that is running.
put_bin() {
  mkdir -p "$(dirname "$2")"
  install -m755 "$1" "$2.new"
  mv -f "$2.new" "$2"
}

# The version an installed `tessera` reports ("0.2.0"), if any.
tessera_version() {
  "$1" --version 2>/dev/null | sed -n 's/^tessera \([^ ]*\).*/\1/p'
}

# ── From source: Rust and a C compiler ────────────────────────────────

rust_minor() { rustc --version 2>/dev/null | sed -n 's/^rustc 1\.\([0-9][0-9]*\).*/\1/p'; }

install_rustup() {
  need_curl
  tmp=$(mktemp -d)
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$tmp/rustup.sh" ||
    fail "Couldn't download the Rust installer from rustup.rs."
  run "Installing Rust" sh "$tmp/rustup.sh" -y --profile minimal || fail "Installing Rust failed."
  rm -rf "$tmp"
  PATH="$HOME/.cargo/bin:$PATH"
}

step_rust() {
  # rustup's Rust first: a system one may be too old.
  if [ -x "$HOME/.cargo/bin/cargo" ]; then PATH="$HOME/.cargo/bin:$PATH"; fi
  m=$(rust_minor)
  if [ -z "$m" ]; then
    if ask "Tessera is built with Rust, which isn't installed. Install it? (rustup.rs, no sudo, about 1.5 GB in your home folder)"; then
      install_rustup
    else
      fail "Rust is needed. Install it with: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    fi
  elif [ "$m" -lt "$RUST_MIN" ]; then
    if command -v rustup >/dev/null 2>&1; then
      if ask "Rust 1.$m is too old (Tessera needs 1.$RUST_MIN or newer). Update it?"; then
        run "Updating Rust" rustup update stable --no-self-update || fail "Updating Rust failed."
        rustup default stable >> "$LOG" 2>&1 || true
      fi
    elif ask "This system's Rust (1.$m) is too old; Tessera needs 1.$RUST_MIN or newer. Install a current one with rustup? (no sudo; the system's stays)"; then
      install_rustup
    fi
  fi
  m=$(rust_minor)
  if [ -z "$m" ] || [ "$m" -lt "$RUST_MIN" ]; then
    fail "Tessera needs Rust 1.$RUST_MIN or newer (found ${m:+1.}${m:-none}). See https://rustup.rs"
  fi
  ok "Rust" "$(rustc --version | cut -d' ' -f2)"
}

step_cc() {
  if ! command -v cc >/dev/null 2>&1; then
    if command -v apt-get >/dev/null 2>&1; then what="build-essential"
    elif command -v dnf >/dev/null 2>&1; then what="gcc"
    elif command -v pacman >/dev/null 2>&1; then what="base-devel"
    elif command -v zypper >/dev/null 2>&1; then what="gcc"
    else fail "Building needs a C compiler. Install gcc, then run this again."
    fi
    if ask "Building needs a C compiler. Install $what?${SUDO:+ (asks for your password)}"; then
      pkg_install "$what" build-essential "gcc gcc-c++ make" base-devel "gcc gcc-c++ make" ||
        fail "Installing $what failed. Install it yourself, then run this again."
    fi
    command -v cc >/dev/null 2>&1 || fail "Install a C compiler (gcc), then run this again."
  fi
  ok "C compiler" "$(cc --version 2>/dev/null | head -n 1 | cut -c1-40)"
}

# ── From source: Tessera and the desktop app ──────────────────────────

build_from_source() {
  step_rust
  step_cc
  TARGET="${CARGO_TARGET_DIR:-$(cd "$ROOT" && cargo metadata --format-version 1 --no-deps 2>/dev/null | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')}"
  [ -n "$TARGET" ] || TARGET="$ROOT/target"
  mkdir -p "$BIN"
  before=$(tessera_version "$BIN/tessera")
  build_web_app
  DETAIL=cargo_detail
  run "Building Tessera" sh -c "cd \"\$1\" && TESSERA_WEB_APP=\"\$2\" cargo build --release --locked -p tessera" sh "$ROOT" "$WEB_APP" ||
    fail "The build failed."
  DETAIL=""
  put_bin "$TARGET/release/tessera" "$BIN/tessera"
  ok "Tessera $(tessera_version "$BIN/tessera")" "$SHOW_BIN/tessera · built in $TOOK$(was "$before")"
  build_desktop
}

# " · was 0.1.0" when an update changed the version.
was() {
  now=$(tessera_version "$BIN/tessera")
  if [ -n "$1" ] && [ "$1" != "$now" ]; then printf ' · was %s' "$1"; fi
}

# Share dependency checks between the web and desktop builds. The marker
# lives with the packages, so deleting node_modules also resets it.
build_interface() (
  cd "$1/apps/tessera/ui" || return 1
  hash=$(sha256_of package-lock.json) || return 1
  installed=$(cat node_modules/.tessera-package-lock.sha256 2>/dev/null) || installed=""
  if [ ! -d node_modules ] || [ "$hash" != "$installed" ]; then
    npm ci --no-bin-links --ignore-scripts --no-audit --no-fund || return 1
    printf '%s\n' "$hash" > node_modules/.tessera-package-lock.sha256 || return 1
  fi
  npm run build
)
# The web app for "Use Tessera in a web browser" (off until turned on in
# Settings), built into the server when Node.js is here. Tessera works
# without it; the setting then says the server has none.
WEB_APP=""
build_web_app() {
  if [ -f "$TOOLCHAIN/env.sh" ]; then . "$TOOLCHAIN/env.sh"; fi
  if ! command -v npm >/dev/null 2>&1; then
    skip "Web app (Use Tessera in a web browser): building it needs Node.js (npm)"
    return 0
  fi
  if run "Building the web app" build_interface "$ROOT"; then
    WEB_APP="$ROOT/apps/tessera/ui/dist"
  else
    warn "Building the web app failed; Tessera works without it (no use in a web browser)."
  fi
}

build_desktop() {
  if [ -n "$NO_DESKTOP" ]; then
    return 0
  elif ! pkg-config --exists webkit2gtk-4.1 2>/dev/null; then
    # Only worth a word where there is a desktop to run it on.
    if [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
      skip "Desktop app: needs webkit2gtk-4.1 (e.g. sudo apt install libwebkit2gtk-4.1-dev)"
    fi
    return 0
  fi
  if [ -f "$TOOLCHAIN/env.sh" ]; then . "$TOOLCHAIN/env.sh"; fi
  if ! command -v npm >/dev/null 2>&1; then
    skip "Desktop app: building it needs Node.js (npm); the phone and web apps work without it"
    return 0
  fi
  if ! cargo tauri --version >/dev/null 2>&1; then
    DETAIL=cargo_detail
    run "Installing the desktop app's build tool" cargo install tauri-cli --version "$TAURI_CLI" --locked ||
      fail "Installing tauri-cli failed."
  fi
  DETAIL=""
  # Built already for the server (the same interface).
  if [ -z "$WEB_APP" ]; then
    run "Building the desktop app's interface" build_interface "$ROOT" ||
      fail "Building the desktop app's interface failed."
  fi
  DETAIL=cargo_detail
  run "Building the desktop app" sh -c 'cd "$1/apps/tessera/src-tauri" && CARGO_TARGET_DIR="$2" cargo tauri build --no-bundle -- --locked' sh "$ROOT" "$TARGET" ||
    fail "Building the desktop app failed."
  DETAIL=""
  put_bin "$TARGET/release/tessera-app" "$BIN/tessera-desktop"
  desktop_entry "$ROOT/apps/tessera/src-tauri/icons/128x128@2x.png" "$ROOT/apps/tessera/src-tauri/icons/32x32.png"
  ok "Desktop app" "$SHOW_BIN/tessera-desktop · in your app menu"
}

# desktop_entry ICON256 ICON32: the app-menu entry for tessera-desktop.
desktop_entry() {
  install -Dm644 "$1" "$HOME/.local/share/icons/hicolor/256x256/apps/tessera.png"
  install -Dm644 "$2" "$HOME/.local/share/icons/hicolor/32x32/apps/tessera.png"
  mkdir -p "$HOME/.local/share/applications"
  cat > "$HOME/.local/share/applications/tessera.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Tessera
Comment=AI agent harness
Exec=$BIN/tessera-desktop
Icon=tessera
Terminal=false
Categories=Development;Utility;
StartupWMClass=tessera-app
DESKTOP
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
  fi
}

# No ready-made build: fetch the source (the release's tag, else the
# default branch) into the cache and build that.
source_from_git() {
  if ! command -v git >/dev/null 2>&1; then
    if ask "Building from source needs git, which isn't installed. Install it?${SUDO:+ (asks for your password)}"; then
      pkg_install git git git git git || true
    fi
    command -v git >/dev/null 2>&1 || fail "Install git, then run this again."
  fi
  ref=$TAG
  if [ -z "$ref" ] && [ -n "$(command -v curl)" ]; then
    # The latest release's tag, from where /releases/latest leads.
    ref=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$REPO_URL/releases/latest" 2>/dev/null | sed -n 's|.*/releases/tag/\([^/?#]*\)$|\1|p') || ref=""
  fi
  dir="$CACHE/source"
  rm -rf "$dir"
  mkdir -p "$CACHE"
  if [ -n "$ref" ]; then
    run "Downloading the source ($ref)" env GIT_TERMINAL_PROMPT=0 git clone --quiet --depth 1 --branch "$ref" "$REPO_URL" "$dir" ||
      fail "Couldn't download Tessera's source from $REPO_URL."
  else
    run "Downloading the source" env GIT_TERMINAL_PROMPT=0 git clone --quiet --depth 1 "$REPO_URL" "$dir" ||
      fail "Couldn't download Tessera's source from $REPO_URL."
  fi
  ROOT=$dir
  # Kept apart from the source, so the next update builds on this one's work.
  if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    CARGO_TARGET_DIR="$CACHE/target"
    export CARGO_TARGET_DIR
  fi
}

# ── Ready-made builds ─────────────────────────────────────────────────

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | sed 's/.*= *//'
  else
    return 1
  fi
}

# The ready-made build for this computer, if there can be one: sets PLAT,
# ARCH and ASSET, or says why not and returns 1.
pick_asset() {
  case "$(uname -m)" in
    x86_64 | amd64) ARCH=x86_64 ;;
    aarch64 | arm64) ARCH=aarch64 ;;
    *) ARCH="" ;;
  esac
  case "$(uname -s)" in
    Linux) PLAT=linux ;;
    Darwin) PLAT=macos ;;
    *) PLAT="" ;;
  esac
  if [ -z "$PLAT" ] || [ -z "$ARCH" ]; then
    WHY="there is no ready-made Tessera for $(uname -s) on $(uname -m)"
    return 1
  fi
  if [ "$PLAT" = linux ]; then
    v=$(getconf GNU_LIBC_VERSION 2>/dev/null | sed -n 's/^glibc 2\.\([0-9]*\).*/\1/p')
    if [ -z "$v" ]; then
      WHY="the ready-made build needs glibc, and this system uses another C library"
      return 1
    elif [ "$v" -lt "$GLIBC_MIN" ]; then
      WHY="the ready-made build needs glibc 2.$GLIBC_MIN or newer (this system has 2.$v)"
      return 1
    fi
  fi
  ASSET="tessera-$PLAT-$ARCH.tar.gz"
}

# The release's assets: ours, or the mirror/test server in TESSERA_DOWNLOAD_URL.
release_base() {
  if [ -n "${TESSERA_DOWNLOAD_URL:-}" ]; then
    printf '%s' "${TESSERA_DOWNLOAD_URL%/}"
  elif [ -n "$TAG" ]; then
    printf '%s/releases/download/%s' "$REPO_URL" "$TAG"
  else
    printf '%s/releases/latest/download' "$REPO_URL"
  fi
}

# verified NAME: download NAME from the release into $TMP and check it
# against SHA256SUMS. Stops the install on a mismatch.
verified() {
  want=$(awk -v f="$1" '$2 == f || $2 == "*" f { print $1; exit }' "$TMP/SHA256SUMS")
  [ -n "$want" ] || return 1
  DL_FILE="$TMP/$1"
  DETAIL=download_detail
  if ! run "Downloading $1" fetch "$BASE/$1" "$TMP/$1"; then
    DETAIL=""
    fail "Downloading $BASE/$1 failed. Check the connection and run this again."
  fi
  DETAIL=""
  got=$(sha256_of "$TMP/$1") || fail "Can't check the download: no sha256sum, shasum or openssl here."
  if [ "$got" != "$want" ]; then
    fail "$1 doesn't match the release's checksum, so nothing was installed. Run this again; if it happens again, please report it."
  fi
  DL_SIZE=$(($(wc -c < "$TMP/$1") / 1048576))
}

# Install the ready-made build and set PREBUILT. Without one for this
# computer it says why and leaves PREBUILT empty: the caller builds from
# source instead. (Called plainly, not in a condition, so `set -e` holds.)
PREBUILT=""
install_prebuilt() {
  if ! pick_asset; then
    skip "No ready-made build: $WHY"
    return 0
  fi
  need_curl
  BASE=$(release_base)
  TMP=$(mktemp -d)
  if ! fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS" 2>> "$LOG"; then
    skip "No ready-made build: no release found at $BASE"
    return 0
  fi
  if ! verified "$ASSET"; then
    skip "No ready-made build for $PLAT-$ARCH in this release"
    return 0
  fi
  mkdir -p "$TMP/x"
  tar -xzf "$TMP/$ASSET" -C "$TMP/x" 2>> "$LOG" || fail "Unpacking $ASSET failed."
  new=$(find "$TMP/x" -type f -name tessera | head -n 1)
  [ -n "$new" ] || fail "$ASSET has no tessera program in it."
  chmod 755 "$new"
  version=$(tessera_version "$new")
  if [ -z "$version" ]; then
    # It can't start here (a library missing, say): building works anyway.
    skip "The ready-made build doesn't run on this system"
    "$new" --version >> "$LOG" 2>&1 || true
    return 0
  fi
  before=$(tessera_version "$BIN/tessera")
  put_bin "$new" "$BIN/tessera"
  ok "Tessera $version" "$SHOW_BIN/tessera · $PLAT-$ARCH · ${DL_SIZE} MB · checksum verified$(was "$before")"
  PREBUILT=1
  install_prebuilt_desktop
}

# The desktop app, when the release has one for this computer and there is
# a desktop to show it on.
install_prebuilt_desktop() {
  if [ -n "$NO_DESKTOP" ] || [ "$PLAT" != linux ] || [ -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
    return 0
  fi
  name="tessera-desktop-$PLAT-$ARCH.tar.gz"
  if ! grep -q "[ *]$name\$" "$TMP/SHA256SUMS"; then
    skip "Desktop app: none ready-made for this computer yet (the phone, terminal and browser apps work)"
    return 0
  fi
  verified "$name"
  mkdir -p "$TMP/d"
  tar -xzf "$TMP/$name" -C "$TMP/d" 2>> "$LOG" || fail "Unpacking $name failed."
  app=$(find "$TMP/d" -type f -name tessera-desktop | head -n 1)
  [ -n "$app" ] || fail "$name has no tessera-desktop program in it."
  # It needs WebKitGTK, a system library most Linux desktops have.
  missing=$(ldd "$app" 2>/dev/null | awk '/not found/ {print $1}' | tr '\n' ' ' | sed 's/ $//')
  if [ -n "$missing" ] && ask "The desktop app needs WebKitGTK 4.1 (a system library). Install it?${SUDO:+ (asks for your password)}"; then
    pkg_install "WebKitGTK" libwebkit2gtk-4.1-0 webkit2gtk4.1 webkit2gtk-4.1 libwebkit2gtk-4_1-0 || true
    missing=$(ldd "$app" 2>/dev/null | awk '/not found/ {print $1}' | tr '\n' ' ' | sed 's/ $//')
  fi
  if [ -n "$missing" ]; then
    skip "Desktop app: this system lacks $missing (WebKitGTK 4.1); install it and run this again"
    return 0
  fi
  put_bin "$app" "$BIN/tessera-desktop"
  icons=$(dirname "$app")/icons
  if [ -f "$icons/256x256.png" ] && [ -f "$icons/32x32.png" ]; then
    desktop_entry "$icons/256x256.png" "$icons/32x32.png"
  fi
  ok "Desktop app" "$SHOW_BIN/tessera-desktop · in your app menu"
}

# ── A browser for web views ───────────────────────────────────────────
# Web views and the agent's browser run in a Chromium-family browser on
# this machine. Sets `browser` to the one Tessera will use, if any.
find_browser() {
  browser=""
  if [ -x /usr/lib/chromium/chromium ]; then browser=/usr/lib/chromium/chromium; fi
  for b in chromium chromium-browser google-chrome-stable google-chrome brave-browser brave microsoft-edge-stable microsoft-edge; do
    [ -z "$browser" ] || break
    found=$(command -v "$b" 2>/dev/null || true)
    [ -n "$found" ] || continue
    # A snap (Ubuntu's chromium-browser is a script that starts one) can't
    # open Tessera's browser profile, which is in a hidden folder.
    if case "$(readlink -f "$found")" in /snap/*) true ;; *) false ;; esac ||
      { [ "$(head -c 2 "$found")" = "#!" ] && grep -q "/snap/" "$found"; }; then
      continue
    fi
    browser=$found
  done
  if [ -z "$browser" ] && [ -x "$BROWSER_DIR/chrome" ]; then browser="$BROWSER_DIR/chrome"; fi
  [ -n "$browser" ]
}

# tight_for_browser: true (and sets `why`) when this computer has too little
# memory or disk for a browser of its own: under about 2 GB of memory (it
# reports under 1.7 GB; the browser takes 350-430 MB while web views are
# open) or under 1.5 GB free where it would go (it takes about 400 MB there).
# The server makes the same call for its default (see crates/core/src/budget.rs).
# kb_size KB: "0.9 GB", "300 MB".
kb_size() {
  awk -v k="$1" 'BEGIN { if (k < 1048576) printf "%d MB", k / 1024; else printf "%.1f GB", k / 1048576 }'
}

tight_for_browser() {
  why=""
  mem_kb=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo 2>/dev/null)
  if [ -n "${TESSERA_MEMORY_MB:-}" ]; then mem_kb=$((TESSERA_MEMORY_MB * 1024)); fi
  if [ -n "$mem_kb" ] && [ "$mem_kb" -lt 1740800 ]; then
    why="this computer has $(kb_size "$mem_kb") of memory, and a browser for web views needs about 400 MB of it"
    return 0
  fi
  dir=$BROWSER_DIR
  while [ ! -d "$dir" ] && [ "$dir" != / ]; do dir=$(dirname "$dir"); done
  free_kb=$(df -Pk "$dir" 2>/dev/null | awk 'NR == 2 {print $4}')
  if [ -n "$free_kb" ] && [ "$free_kb" -lt 1572864 ]; then
    why="only $(kb_size "$free_kb") of disk is free here, and a browser for web views takes about 400 MB"
    return 0
  fi
  return 1
}

# Libraries the downloaded browser can't find.
missing_libs() {
  ldd "$BROWSER_DIR/chrome" 2>/dev/null | awk '/not found/ {print $1}' | tr '\n' ' ' | sed 's/ $//'
}

# The system packages Chrome needs, for apt (its own list when it has one).
browser_deps() {
  deps="$(dirname "$(readlink -f "$BROWSER_DIR/chrome")")/deb.deps"
  if [ -f "$deps" ]; then
    grep -v '^ *$' "$deps" | paste -sd, -
  else
    echo "libasound2, libatk-bridge2.0-0, libatk1.0-0, libatspi2.0-0, libcups2, libdbus-1-3, libdrm2, libgbm1, libglib2.0-0, libnspr4, libnss3, libpango-1.0-0, libx11-6, libxcb1, libxcomposite1, libxdamage1, libxext6, libxfixes3, libxkbcommon0, libxrandr2"
  fi
}

# Chrome for Testing's name for this computer, when Google builds it here.
chrome_platform() {
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64 | Linux-amd64) echo linux64 ;;
    Linux-aarch64 | Linux-arm64) echo linux-arm64 ;;
    *) return 1 ;;
  esac
}

# The latest stable Chrome for Testing ("154.0.8037.92"): one small request.
latest_chrome() {
  v=$(curl --proto '=https' --tlsv1.2 -fsSL --max-time 15 https://googlechromelabs.github.io/chrome-for-testing/LATEST_RELEASE_STABLE 2>/dev/null) || v=""
  case "$v" in '' | *[!0-9.]*) return 1 ;; esac
  printf '%s' "$v"
}

# The version of the Chrome for Testing in $BROWSER_DIR, if that's what is there.
chrome_version() {
  readlink "$BROWSER_DIR/chrome" 2>/dev/null | sed -n 's|^\([0-9][0-9.]*\)/chrome-linux[^/]*/chrome$|\1|p'
}

# newer A B: version A is newer than version B.
newer() {
  [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n -k4,4n | tail -n 1)" = "$1" ]
}

# install_browser [VERSION]: Chrome for Testing (Google's portable Chrome
# build) into $BROWSER_DIR, the latest stable one unless VERSION is given.
install_browser() {
  if ! plat=$(chrome_platform); then
    skip "Web views: install Chromium or Google Chrome with your package manager"
    return 0
  fi
  need_curl
  ver=${1:-}
  if [ -z "$ver" ] && ! ver=$(latest_chrome); then
    warn "Couldn't look up the latest Chrome; run this again later for web views."
    return 0
  fi
  prev=$(chrome_version)
  tmp=$(mktemp -d)
  DL_FILE="$tmp/chrome.zip"
  DETAIL=download_detail
  # Google publishes no checksums for these builds: the download is checked
  # by HTTPS to Google's own storage (fetch allows nothing else).
  if ! run "Downloading Chrome for Testing $ver" fetch "https://storage.googleapis.com/chrome-for-testing-public/$ver/$plat/chrome-$plat.zip" "$tmp/chrome.zip"; then
    DETAIL=""
    rm -rf "$tmp"
    warn "The browser download failed; run this again later for web views."
    return 0
  fi
  DETAIL=""
  mkdir -p "$BROWSER_DIR"
  rm -rf "${BROWSER_DIR:?}/$ver"
  if command -v unzip >/dev/null 2>&1; then
    run "Unpacking the browser" unzip -q "$tmp/chrome.zip" -d "$BROWSER_DIR/$ver" || true
  elif command -v python3 >/dev/null 2>&1; then
    # Python's zipfile drops permission bits: put them back.
    run "Unpacking the browser" python3 -c '
import os, sys, zipfile
with zipfile.ZipFile(sys.argv[1]) as z:
    for info in z.infolist():
        path = z.extract(info, sys.argv[2])
        mode = (info.external_attr >> 16) & 0o777
        if mode:
            os.chmod(path, mode)
' "$tmp/chrome.zip" "$BROWSER_DIR/$ver" || true
  else
    rm -rf "$tmp"
    warn "Unpacking the browser needs unzip or python3; install one and run this again for web views."
    return 0
  fi
  rm -rf "$tmp"
  if [ ! -x "$BROWSER_DIR/$ver/chrome-$plat/chrome" ]; then
    rm -rf "${BROWSER_DIR:?}/$ver"
    warn "Unpacking the browser failed; run this again later for web views."
    return 0
  fi
  ln -sfn "$ver/chrome-$plat/chrome" "$BROWSER_DIR/chrome"
  # Earlier downloads go, except the one just replaced: a running Tessera
  # may use it until it restarts (the next update removes it).
  for d in "$BROWSER_DIR"/*/; do
    case "$d" in
      "$BROWSER_DIR/$ver/" | "$BROWSER_DIR/$prev/") ;;
      *) rm -rf "$d" ;;
    esac
  done
  if [ -n "$prev" ] && [ "$prev" != "$ver" ]; then
    ok "Browser for web views" "Chrome for Testing $ver · updated from $prev"
  else
    ok "Browser for web views" "Chrome for Testing $ver"
  fi
  missing=$(missing_libs)
  if [ -n "$missing" ] && command -v apt-get >/dev/null 2>&1 &&
    ask "It needs a few system libraries ($(echo "$missing" | wc -w | tr -d ' ') missing). Install them?${SUDO:+ (asks for your password)}"; then
    if sudo_ready; then
      run "Installing the browser's libraries" $SUDO env DEBIAN_FRONTEND=noninteractive apt-get satisfy -y -qq --no-install-recommends "$(browser_deps)" || true
    fi
    missing=$(missing_libs)
    if [ -z "$missing" ]; then ok "Browser libraries"; fi
  fi
  if [ -n "$missing" ]; then
    warn "Web views need system libraries that are missing: $missing"
    note "On Debian or Ubuntu: sudo apt-get satisfy \"$(browser_deps)\""
  fi
}

# Chrome for Testing doesn't update itself: look for a newer stable one and
# offer it (the browser web views run in should get security fixes).
update_browser() {
  have=$(chrome_version)
  if [ -z "$have" ]; then
    ok "Browser for web views" "$browser"
    return 0
  fi
  latest=$(latest_chrome) || latest=""
  if [ -z "$latest" ]; then
    ok "Browser for web views" "Chrome for Testing $have"
    note "Couldn't check for a newer one; run this again later."
  elif ! newer "$latest" "$have"; then
    ok "Browser for web views" "Chrome for Testing $have · up to date"
  elif ask "A newer Chrome for Testing is out ($latest; this one is $have). Update it? (about 170 MB)"; then
    install_browser "$latest"
  else
    skip "Web views: Chrome for Testing $have (run this again to update it)"
  fi
}

step_browser() {
  if [ "$(uname -s)" = Darwin ]; then
    # Tessera finds these in /Applications (see browser/process.rs).
    browser=""
    for b in "Google Chrome" Chromium "Brave Browser" "Microsoft Edge"; do
      if [ -z "$browser" ] && [ -d "/Applications/$b.app" ]; then browser=$b; fi
    done
    if [ -n "$browser" ]; then
      ok "Browser for web views" "$browser"
    else
      skip "Web views: they use Google Chrome or Chromium, if you install one"
    fi
  elif find_browser; then
    if [ "$browser" = "$BROWSER_DIR/chrome" ]; then
      update_browser
    else
      ok "Browser for web views" "$browser"
    fi
  elif tight_for_browser; then
    # Web views can still open on another computer that runs Tessera.
    note "Not downloading a browser for web views by default: $why."
    if ask_no "Download one anyway? (Chrome for Testing from Google; about 170 MB, no sudo)"; then
      install_browser
    else
      skip "Web views: open them on another computer of yours, or run this again to download a browser"
    fi
  elif ask "Web views (and the agent's browser) need a browser. Download one? (Chrome for Testing from Google; about 170 MB, no sudo)"; then
    install_browser
  else
    skip "Web views: no browser yet; run this again any time to download one"
  fi
}

# ── An update takes effect at once ────────────────────────────────────

step_service() {
  command -v systemctl >/dev/null 2>&1 || return 0
  # Units written before KillMode=mixed sent the stop signal to everything the
  # server runs: Claude Code took it for the user stopping its turn, and the
  # turn couldn't continue after the restart. Add the line the new units have.
  for unit in "$HOME/.config/systemd/user/tessera.service" /etc/systemd/system/tessera.service; do
    if [ -w "$unit" ] && grep -q '^KillSignal=SIGINT' "$unit" && ! grep -q '^KillMode=' "$unit"; then
      sed -i 's/^KillSignal=SIGINT$/KillSignal=SIGINT\nKillMode=mixed/' "$unit"
      case "$unit" in
        /etc/*) systemctl daemon-reload 2>/dev/null ;;
        *) systemctl --user daemon-reload 2>/dev/null ;;
      esac
    fi
  done
  # Units written before the server's watchdog: let it tell systemd it still
  # runs (it switches the watchdog on itself, so older servers are unaffected).
  for unit in "$HOME/.config/systemd/user/tessera.service" /etc/systemd/system/tessera.service; do
    if [ -w "$unit" ] && grep -q '^KillMode=mixed' "$unit" && ! grep -q '^NotifyAccess=' "$unit"; then
      sed -i 's/^KillMode=mixed$/KillMode=mixed\nNotifyAccess=main/' "$unit"
      case "$unit" in
        /etc/*) systemctl daemon-reload 2>/dev/null ;;
        *) systemctl --user daemon-reload 2>/dev/null ;;
      esac
    fi
  done
  # Units written before OOMPolicy=continue went down whole when anything the
  # server ran (a browser, a build) was killed for running out of memory.
  for unit in "$HOME/.config/systemd/user/tessera.service" /etc/systemd/system/tessera.service; do
    if [ -w "$unit" ] && grep -q '^KillMode=mixed' "$unit" && ! grep -q '^OOMPolicy=' "$unit"; then
      sed -i 's/^KillMode=mixed$/KillMode=mixed\nOOMPolicy=continue/' "$unit"
      case "$unit" in
        /etc/*) systemctl daemon-reload 2>/dev/null ;;
        *) systemctl --user daemon-reload 2>/dev/null ;;
      esac
    fi
  done
  if [ -z "$SUDO" ] && systemctl is-active --quiet tessera 2>/dev/null; then
    systemctl restart tessera && ok "Background service" "restarted with the new version"
  elif systemctl --user is-active --quiet tessera 2>/dev/null; then
    systemctl --user restart tessera && ok "Background service" "restarted with the new version"
  fi
}

# ── Next ──────────────────────────────────────────────────────────────

step_next() {
  printf "\n  ${B}Tessera is installed.${R}\n"
  case ":$PATH:" in
    *":$BIN:"*) on_path=1 ;;
    *) on_path="" ;;
  esac
  if [ -n "$ANSWER_FROM" ] && [ -n "$TTY" ] && ask "Set it up now? (AI models, your phone, running in the background)"; then
    if [ -z "$on_path" ]; then note "$SHOW_BIN isn't on your PATH yet: until you log in again, run $SHOW_BIN/tessera"; fi
    cleanup
    trap - EXIT INT TERM
    exec "$BIN/tessera" setup < "$ANSWER_FROM"
  fi
  next="tessera setup"
  if [ -z "$on_path" ]; then
    note "$SHOW_BIN isn't on your PATH yet: most systems add it when you log in again; until then, run $SHOW_BIN/tessera."
    next="$SHOW_BIN/tessera setup"
  fi
  printf "  Next: ${GOLD}%s${R} ${DIM}walks you through models, your phone and the background service.${R}\n\n" "$next"
}

# ── Main ──────────────────────────────────────────────────────────────

main() {
  while [ $# -gt 0 ]; do
    case "$1" in
      --no-desktop) NO_DESKTOP=1 ;;
      --yes | -y) YES=1 ;;
      --source) FROM_SOURCE=1 ;;
      --version)
        [ $# -gt 1 ] || { printf 'Missing the version after --version (e.g. v0.2.0)\n' >&2; exit 2; }
        TAG=$2
        shift ;;
      --version=*) TAG=${1#--version=} ;;
      -h | --help) usage; exit 0 ;;
      *) printf 'Unknown option: %s (try --help)\n' "$1" >&2; exit 2 ;;
    esac
    shift
  done
  case "$TAG" in '' | v*) ;; *) TAG="v$TAG" ;; esac
  mkdir -p "$CACHE"
  : > "$LOG"
  setup_output
  trap 'cleanup; exit 130' INT TERM
  trap cleanup EXIT
  SHOW_BIN=$(printf '%s' "$BIN" | sed "s|^$HOME|~|")

  # A copy of the source around this script: build that (how a checkout is
  # installed and updated). Otherwise (piped from curl, say) a ready-made
  # build, or the source of the release when there is none for this computer.
  ROOT=""
  case "$0" in
    *install.sh)
      d=$(cd "$(dirname "$0")/.." 2>/dev/null && pwd) || d=""
      if [ -n "$d" ] && [ -f "$d/Cargo.toml" ] && [ -d "$d/crates/cli" ]; then ROOT=$d; fi ;;
  esac

  if [ -n "$TTY" ] && [ -n "$UTF" ]; then
    if [ -n "$ROOT" ]; then sub="installer · builds this copy, then a guided setup"; else sub="installer · everything it needs, then a guided setup"; fi
    printf "\n  ${GOLD}▗▄▖▗▄▖${R} ${B}Tessera${R}\n  ${GOLD}▝▀▘▝█▘${R} ${DIM}%s${R}\n\n" "$sub"
  else
    printf '\nTessera installer\n\n'
  fi

  if [ -n "$ROOT" ]; then
    build_from_source
  else
    if [ -z "$FROM_SOURCE" ]; then install_prebuilt; fi
    if [ -z "$PREBUILT" ]; then
      source_from_git
      build_from_source
    fi
  fi
  step_browser
  step_service
  step_next
}

main "$@"
