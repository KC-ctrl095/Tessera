#!/bin/sh
# Sign the benchmark's own Tessera home into ChatGPT (device code), so GPT
# runs go through Tessera's openai-codex provider with a login of their own.
#
#   sh bench/login-chatgpt.sh          print a URL and a code, wait for approval
#   sh bench/login-chatgpt.sh --check  say whether the bench home is signed in
#
# Never copy tokens from ~/.codex or another Tessera home instead: two clients
# sharing one refresh token can sign each other out. This makes a separate
# login (its own refresh token) in BENCH_HOME (default ~/.cache/tessera/bench-home).
# bench/run.py copies that home's auth.json into each run's throwaway home and
# copies it back when Tessera refreshed it, so keep GPT runs at one at a time
# (run.py does).
set -eu

BIN=${TESSERA_BIN:-target/release/tessera}
BENCH_HOME=${BENCH_HOME:-$HOME/.cache/tessera/bench-home}

if [ ! -x "$BIN" ]; then
    echo "no tessera binary at $BIN (set TESSERA_BIN)" >&2
    exit 1
fi
mkdir -p "$BENCH_HOME/xdg/config" "$BENCH_HOME/xdg/data" "$BENCH_HOME/xdg/cache" "$BENCH_HOME/xdg/state"
chmod 700 "$BENCH_HOME"

# Everything this Tessera writes stays inside the bench home.
run() {
    env -i HOME="$HOME" PATH="$PATH" LANG="${LANG:-C.UTF-8}" TERM=dumb \
        TESSERA_HOME="$BENCH_HOME" \
        XDG_CONFIG_HOME="$BENCH_HOME/xdg/config" XDG_DATA_HOME="$BENCH_HOME/xdg/data" \
        XDG_CACHE_HOME="$BENCH_HOME/xdg/cache" XDG_STATE_HOME="$BENCH_HOME/xdg/state" \
        "$BIN" --local --plain --home "$BENCH_HOME" "$@"
}

if [ "${1:-}" = "--check" ]; then
    run providers | grep -i -A3 "chatgpt\|openai-codex" || true
    if [ -s "$BENCH_HOME/auth.json" ] && grep -q '"openai-codex"' "$BENCH_HOME/auth.json"; then
        echo "signed in: $BENCH_HOME/auth.json has a ChatGPT login"
    else
        echo "not signed in yet: run sh bench/login-chatgpt.sh"
        exit 1
    fi
    exit 0
fi

echo "Signing $BENCH_HOME into ChatGPT. Open the URL below on any device, sign in, and type the code."
run login chatgpt
