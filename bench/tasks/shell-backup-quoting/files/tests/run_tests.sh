#!/bin/sh
# Runs backup.sh against throwaway folders:  sh tests/run_tests.sh
here=$(cd "$(dirname "$0")/.." && pwd)
script="$here/backup.sh"
# Run it the way its first line says (works even without the x bit).
interp=$(sed -n '1s/^#![[:space:]]*//p' "$script")
backup() { ${interp:-sh} "$script" "$@"; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0
ok() { echo "ok   $1"; }
bad() { echo "FAIL $1"; fails=$((fails + 1)); }
export BACKUP_DATE=20260115

# 1. Folder names with spaces.
mkdir -p "$tmp/src/Tax returns" "$tmp/src/photos" "$tmp/src/Old Projects/site"
echo 2025 > "$tmp/src/Tax returns/2025.txt"
echo meow > "$tmp/src/photos/cat.jpg"
echo "<html>" > "$tmp/src/Old Projects/site/index.html"
echo loose > "$tmp/src/notes.txt"
if backup "$tmp/src" "$tmp/out" > "$tmp/log" 2>&1; then ok "exit status 0"; else bad "exit status: $(cat "$tmp/log")"; fi
for name in "Tax returns" photos "Old Projects"; do
  if [ -f "$tmp/out/$name-20260115.tar.gz" ]; then ok "archive for $name"; else bad "no archive for $name"; fi
done
count=$(ls "$tmp/out" 2>/dev/null | wc -l)
if [ "$count" -eq 3 ]; then ok "three archives"; else bad "expected 3 archives, found $count"; fi
mkdir "$tmp/x"
tar -xzf "$tmp/out/Tax returns-20260115.tar.gz" -C "$tmp/x" 2>/dev/null
if [ "$(cat "$tmp/x/Tax returns/2025.txt" 2>/dev/null)" = 2025 ]; then ok "the archive holds the folder"; else bad "the archive should hold Tax returns/2025.txt"; fi

# 2. A source folder that doesn't exist.
mkdir -p "$tmp/cwd/stray"
(cd "$tmp/cwd" && backup "$tmp/missing" "$tmp/out2" > "$tmp/stdout" 2> "$tmp/stderr")
code=$?
if [ "$code" -eq 1 ]; then ok "missing source: exit 1"; else bad "missing source: exit $code, expected 1"; fi
if [ -s "$tmp/stderr" ]; then ok "missing source: error on stderr"; else bad "missing source: no error on stderr"; fi
if grep -q done "$tmp/stdout"; then bad "missing source: says done"; else ok "missing source: doesn't say done"; fi
if ls "$tmp/out2" 2>/dev/null | grep -q .; then bad "missing source: made archives anyway"; else ok "missing source: no archives"; fi

# 3. Relative paths, as cron users type them.
mkdir -p "$tmp/rel/data/inbox"
echo hi > "$tmp/rel/data/inbox/a.txt"
(cd "$tmp/rel" && backup data backups > /dev/null 2>&1)
if [ -f "$tmp/rel/backups/inbox-20260115.tar.gz" ]; then ok "relative paths"; else bad "relative paths: expected backups/inbox-20260115.tar.gz next to data/"; fi

if [ "$fails" -eq 0 ]; then echo "all tests passed"; else echo "$fails test(s) failed"; fi
[ "$fails" -eq 0 ]
