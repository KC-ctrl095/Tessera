set -e
python3 - <<'PY'
p = "backup.sh"
s = open(p).read()
old = s[s.index("mkdir -p $DEST"):]
new = '''if [ ! -d "$SRC" ]; then
  echo "backup.sh: source folder not found: $SRC" >&2
  exit 1
fi

mkdir -p -- "$DEST" || exit 1
# Absolute, so it still points to the right place after the cd below.
DEST=$(cd -- "$DEST" && pwd) || exit 1
cd -- "$SRC" || exit 1
for d in *; do
  if [ -d "$d" ]; then
    tar -czf "$DEST/$d-$DATE.tar.gz" -- "$d" || exit 1
    echo "backed up $d"
  fi
done
echo "done"
'''
open(p, "w").write(s.replace(old, new))
PY
