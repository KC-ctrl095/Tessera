#!/bin/sh
# rotate_logs.sh DIR KEEP: rotates and prunes the logs in DIR (see docs/ROTATION.md).

usage() {
  echo "usage: rotate_logs.sh DIR KEEP" >&2
  exit 2
}

[ $# -eq 2 ] || usage
dir=$1
keep=$2
case $keep in
  '' | *[!0-9]*) usage ;;
esac
[ "$keep" -gt 0 ] 2>/dev/null || usage
[ -d "$dir" ] || usage

# 1. Rotate every non-empty regular .log file.
for f in "$dir"/*.log; do
  [ -f "$f" ] && [ -s "$f" ] || continue
  day=$(date -u -r "$f" +%Y%m%d) || exit 1
  out="$f.$day.gz"
  n=2
  while [ -e "$out" ]; do
    out="$f.$day-$n.gz"
    n=$((n + 1))
  done
  gzip -c < "$f" > "$out" || { rm -f -- "$out"; exit 1; }
  # Truncate in place: the app keeps the file open.
  : > "$f"
done

# 2. Keep the KEEP newest archives of every log (by the date and number in the name).
for f in "$dir"/*.log; do
  [ -f "$f" ] || continue
  for a in "$f".*.gz; do
    [ -f "$a" ] || continue
    tag=${a#"$f".}
    tag=${tag%.gz}
    case $tag in
      [0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]) day=$tag num=1 ;;
      [0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]-*)
        day=${tag%%-*}
        num=${tag#*-}
        case $num in '' | *[!0-9]*) continue ;; esac
        ;;
      *) continue ;;
    esac
    printf '%s %012d\t%s\n' "$day" "$num" "$a"
  done | sort -r | tail -n +"$((keep + 1))" | cut -f 2- | while IFS= read -r old; do
    rm -f -- "$old"
  done
done
exit 0
