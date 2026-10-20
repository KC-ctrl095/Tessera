set -e
git merge -q feature/retries >/dev/null 2>&1 || true
python3 - <<'PY'
import re

def resolve(path, pick):
    s = open(path).read()
    pat = re.compile(r"<<<<<<< [^\n]*\n(.*?)=======\n(.*?)>>>>>>> [^\n]*\n", re.S)
    open(path, "w").write(pat.sub(lambda m: pick(m.group(1), m.group(2)), s))

# config: main's TIMEOUT line, then the feature's retry settings.
resolve("config.py", lambda ours, theirs: ours + "".join(l + "\n" for l in theirs.splitlines() if not l.startswith("TIMEOUT")))
# changelog: the feature's entry (2026-09-04) is newer than main's (2026-09-02).
resolve("CHANGELOG.md", lambda ours, theirs: theirs + "\n" + ours)
PY
git add config.py CHANGELOG.md
git commit -q --no-edit
