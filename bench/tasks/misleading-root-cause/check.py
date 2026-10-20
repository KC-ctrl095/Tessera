import shutil

from checklib import FILES, WORK, Check, copy_files, diff_lines, run, tail, with_hidden

c = Check()
# Adding tests is fine; changing or removing the existing ones isn't (the
# scored run below uses the original tests either way).
weakened = []
for p in sorted((FILES / "tests").rglob("*")):
    if p.is_file():
        rel = p.relative_to(FILES).as_posix()
        if not (WORK / rel).is_file() or diff_lines(rel)[0]:
            weakened.append(rel)
c.ok(not weakened, "the existing tests are intact" if not weakened else f"existing tests were changed or removed: {', '.join(weakened)}")

with with_hidden() as d:
    # The original data and tests, plus the hidden ones; tests the agent added are left out.
    shutil.rmtree(d / "data", ignore_errors=True)
    copy_files(FILES / "data", d / "data")
    keep = {p.relative_to(FILES).as_posix() for p in (FILES / "tests").rglob("*") if p.is_file()}
    keep.add("tests/test_lisbon_hidden.py")
    for p in list((d / "tests").rglob("*.py")):
        if p.relative_to(d).as_posix() not in keep:
            p.unlink()
    copy_files(FILES / "tests", d / "tests")
    code, out = run("python3 -m unittest discover -s tests -t . -v", cwd=d, timeout=90)
failed = [l for l in out.splitlines() if l.endswith(("... FAIL", "... ERROR"))]
c.ok(code == 0, "the project's tests and the tests of every consumer of the clock helpers pass" if code == 0
     else "tests fail:\n  " + "\n  ".join(failed[:20]) + "\n" + tail(out, 2500))
c.done()
