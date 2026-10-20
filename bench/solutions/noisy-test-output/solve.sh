# Three separate bugs behind the three failing tests (found with
# `python3 run_tests.py > out.txt; grep -nE '\.\.\. (FAIL|ERROR) ' out.txt`).
set -e
python3 - <<'PY'
from pathlib import Path

def fix(path, old, new):
    p = Path(path)
    s = p.read_text()
    assert old in s, (path, old)
    p.write_text(s.replace(old, new))

# A run of separators (and hyphens) must become one hyphen: "ab  104" -> "AB-104".
fix("dockyard/catalog.py", '_SEPARATORS = re.compile(r"[\\s_.]")', '_SEPARATORS = re.compile(r"[\\s_.\\-]+")')
# holidays.json lists each country's holidays under "dates".
fix("dockyard/schedule.py", 'entry.get("days", [])', 'entry.get("dates", [])')
# Band limits are inclusive: a parcel of exactly the limit belongs to that band.
fix("dockyard/rates.py", "bisect.bisect_right(limits, weight_g)", "bisect.bisect_left(limits, weight_g)")
PY
