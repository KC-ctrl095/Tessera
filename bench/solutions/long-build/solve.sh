set -e
python3 - <<'PY'
import json
r = json.load(open("release.json"))
r["version"] = "2.4.1"
r["released"] = "2026-09-28"
open("release.json", "w").write(json.dumps(r, indent=2) + "\n")
p = "src/readlist/core.py"
s = open(p).read()
s = s.replace('title, author, pages = (part.strip() for part in line.split("|"))', 'title, author, pages = (part.strip() for part in line.rsplit("|", 2))')
open(p, "w").write(s)
PY
./build.sh
