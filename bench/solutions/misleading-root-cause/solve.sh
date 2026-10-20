set -e
# The root cause: last_sunday() returns the 1st of the next month when that
# day is a Sunday (1 November 2026), so summer time ran a week too long in 2026.
# The BIL-212 hotfix in billing/due_dates.py compensated for it and has to go.
python3 - <<'PY'
from pathlib import Path
p = Path("utils/tz.py")
s = p.read_text()
old = "    return first_of_next - timedelta(days=(first_of_next.weekday() + 1) % 7)\n"
assert old in s
p.write_text(s.replace(old, "    return first_of_next - timedelta(days=first_of_next.weekday() + 1)\n"))
p = Path("billing/due_dates.py")
s = p.read_text()
start = s.index("# BIL-212")
end = s.index("def due_day")
s = s[:start] + s[end:]
s = s.replace("    if _BIL_212[0] <= day <= _BIL_212[1]:\n        due += timedelta(hours=1)\n", "")
s = s.replace("    due = to_utc(datetime.combine(day, END_OF_DAY))\n    return due\n", "    return to_utc(datetime.combine(day, END_OF_DAY))\n")
s = s.replace("from datetime import date, datetime, time, timedelta\n", "from datetime import date, datetime, time, timedelta  # noqa: F401\n")
p.write_text(s)
PY
