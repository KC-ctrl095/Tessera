import difflib

from checklib import HIDDEN, WORK, Check, original_tree_digest, run, tail, tree_digest, with_hidden

c = Check()
c.ok(tree_digest(WORK / "expected") == original_tree_digest("expected"), "expected/ is unchanged")
c.ok(tree_digest(WORK / "data") == original_tree_digest("data"), "data/ is unchanged")


def compare(d, trips, stations, want_file, what):
    code, out = run(["python3", "-m", "tripstats", trips, stations, "September 2026"], cwd=d, timeout=120)
    want = (HIDDEN / want_file).read_text(encoding="utf-8")
    if code != 0:
        c.fail(f"{what}: the pipeline fails:\n{tail(out)}")
        return
    if out == want:
        c.note(f"{what}: the report matches")
        return
    diff = list(difflib.unified_diff(want.splitlines(), out.splitlines(), "expected", "got", lineterm="", n=0))
    c.fail(f"{what}: the report differs ({sum(1 for l in diff if l.startswith('+') and not l.startswith('+++'))} lines), e.g.\n" + "\n".join(diff[:12]))


with with_hidden(overlay=HIDDEN / "none") as d:
    compare(d, "data/trips.csv", "data/stations.json", "expected_report.txt", "September data")
    compare(d, str(HIDDEN / "data2" / "trips.csv"), str(HIDDEN / "data2" / "stations.json"), "data2/expected.txt", "another month's data")
c.done()
