set -e
python3 - <<'PY'
import json
import re
from collections import Counter
from datetime import datetime, timezone

line_re = re.compile(r'\[([^\]]+)\] "(\S+) (\S+) [^"]*" (\d{3}) ')
start = datetime(2026, 9, 12, 14, tzinfo=timezone.utc)
end = datetime(2026, 9, 12, 15, tzinfo=timezone.utc)
months = {m: i + 1 for i, m in enumerate("Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec".split())}
counts = Counter()
with open("access.log", encoding="utf-8", errors="replace") as f:
    for line in f:
        m = line_re.search(line)
        if not m:
            continue
        stamp, _method, target, status = m.groups()
        d = re.fullmatch(r"(\d+)/(\w+)/(\d+):(\d+):(\d+):(\d+) ([+-]\d{4})", stamp)
        if not d:
            continue
        day, mon, year, hh, mm, ss, off = d.groups()
        t = datetime.strptime(f"{year}-{months[mon]:02d}-{day} {hh}:{mm}:{ss} {off}", "%Y-%m-%d %H:%M:%S %z")
        if 500 <= int(status) <= 599 and start <= t < end:
            counts[target.split("?", 1)[0]] += 1
json.dump([{"path": p, "count": n} for p, n in counts.most_common(3)], open("answer.json", "w"), indent=2)
PY
