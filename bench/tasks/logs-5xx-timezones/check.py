import importlib.util
import json

from checklib import TASK, WORK, Check, original_text

spec = importlib.util.spec_from_file_location("make_log", TASK / "make_log.py")
make_log = importlib.util.module_from_spec(spec)
spec.loader.exec_module(make_log)

c = Check()

# The truth comes from the log as handed out, not the work copy.
log = original_text("access.log")
truth, problems = make_log.verify(log)
if problems:
    c.fail(f"task data is unusable: {problems}")
    c.done()
want = [{"path": p, "count": n} for p, n in truth]

path = WORK / "answer.json"
if not c.ok(path.is_file(), "answer.json exists" if path.is_file() else "answer.json is missing"):
    c.done()
try:
    got = json.loads(path.read_text(encoding="utf-8"))
except ValueError as e:
    c.fail(f"answer.json isn't valid JSON: {e}")
    c.done()


def as_count(v):
    if isinstance(v, bool):
        return None
    if isinstance(v, int):
        return v
    if isinstance(v, float) and v.is_integer():
        return int(v)
    if isinstance(v, str) and v.strip().isdigit():
        return int(v.strip())
    return None


rows = None
if isinstance(got, list) and all(isinstance(x, dict) and "path" in x and "count" in x for x in got):
    rows = [{"path": x["path"], "count": as_count(x["count"])} for x in got]
c.ok(rows is not None, "answer.json is a list of {path, count} objects" if rows is not None else f"answer.json should be a list of objects with path and count, got: {json.dumps(got)[:300]}")
if rows is not None:
    c.ok(rows == want, "the three endpoints and their counts are right, most first" if rows == want else f"expected {json.dumps(want)}, got {json.dumps(rows)}")
c.done()
