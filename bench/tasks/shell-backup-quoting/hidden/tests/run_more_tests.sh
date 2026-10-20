#!/bin/sh
# More cases for backup.sh:  sh tests/run_more_tests.sh
here=$(cd "$(dirname "$0")/.." && pwd)
script="$here/backup.sh"
interp=$(sed -n '1s/^#![[:space:]]*//p' "$script")
backup() { ${interp:-sh} "$script" "$@"; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0
ok() { echo "ok   $1"; }
bad() { echo "FAIL $1"; fails=$((fails + 1)); }
export BACKUP_DATE=20260301

# holds ARCHIVE PATH CONTENT: the archive unpacks PATH with CONTENT.
holds() {
  rm -rf "$tmp/x" && mkdir "$tmp/x"
  if tar -xzf "$1" -C "$tmp/x" 2>/dev/null && [ "$(cat "$tmp/x/$2" 2>/dev/null)" = "$3" ]; then
    ok "$(basename "$1") holds $2"
  else
    bad "$(basename "$1") should hold $2"
  fi
}

# Awkward names: a leading dash, glob characters (with a folder the pattern
# would match), two spaces in a row, a destination with spaces.
mkdir -p "$tmp/src/-draft" "$tmp/src/set[1]" "$tmp/src/set1" "$tmp/src/a  b" "$tmp/src/Music/Live 2024" "$tmp/src/x*y"
echo d > "$tmp/src/-draft/plan.txt"
echo bracket > "$tmp/src/set[1]/which.txt"
echo plain > "$tmp/src/set1/which.txt"
echo two > "$tmp/src/a  b/f.txt"
echo song > "$tmp/src/Music/Live 2024/track 01.txt"
echo star > "$tmp/src/x*y/s.txt"
echo loose > "$tmp/src/readme.txt"
if backup "$tmp/src" "$tmp/my backups" > "$tmp/log" 2>&1; then ok "exit status 0"; else bad "exit status: $(cat "$tmp/log")"; fi
holds "$tmp/my backups/-draft-20260301.tar.gz" "-draft/plan.txt" d
holds "$tmp/my backups/set[1]-20260301.tar.gz" "set[1]/which.txt" bracket
holds "$tmp/my backups/set1-20260301.tar.gz" "set1/which.txt" plain
holds "$tmp/my backups/a  b-20260301.tar.gz" "a  b/f.txt" two
holds "$tmp/my backups/Music-20260301.tar.gz" "Music/Live 2024/track 01.txt" song
holds "$tmp/my backups/x*y-20260301.tar.gz" "x*y/s.txt" star
count=$(ls "$tmp/my backups" 2>/dev/null | wc -l)
if [ "$count" -eq 6 ]; then ok "six archives"; else bad "expected 6 archives, found $count: $(ls "$tmp/my backups" 2>/dev/null | tr '\n' '|')"; fi

# A trailing slash on the source.
mkdir -p "$tmp/src2/docs"
echo x > "$tmp/src2/docs/x.txt"
backup "$tmp/src2/" "$tmp/out2" > /dev/null 2>&1
holds "$tmp/out2/docs-20260301.tar.gz" "docs/x.txt" x

# An empty source folder: nothing to do, not an error.
mkdir -p "$tmp/empty"
if backup "$tmp/empty" "$tmp/out3" > /dev/null 2>&1; then ok "empty source: exit 0"; else bad "empty source should exit 0"; fi
if ls "$tmp/out3" 2>/dev/null | grep -q .; then bad "empty source: made archives: $(ls "$tmp/out3")"; else ok "empty source: no archives"; fi

# The source is a file, not a folder.
echo hi > "$tmp/afile"
(cd "$tmp" && backup "$tmp/afile" "$tmp/out4" > "$tmp/stdout" 2> "$tmp/stderr")
code=$?
if [ "$code" -eq 1 ]; then ok "source is a file: exit 1"; else bad "source is a file: exit $code, expected 1"; fi
if [ -s "$tmp/stderr" ]; then ok "source is a file: error on stderr"; else bad "source is a file: no error on stderr"; fi
if ls "$tmp/out4" 2>/dev/null | grep -q .; then bad "source is a file: made archives"; else ok "source is a file: no archives"; fi

if [ "$fails" -eq 0 ]; then echo "all tests passed"; else echo "$fails test(s) failed"; fi
[ "$fails" -eq 0 ]
