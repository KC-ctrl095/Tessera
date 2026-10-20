import re

from checklib import WORK, Check, run, tail, with_hidden

c = Check()
api = (WORK / "docs" / "API.md").read_text() if (WORK / "docs" / "API.md").exists() else ""
c.ok("limit" in api and "after" in api and "next_cursor" in api, "docs/API.md describes limit, after and next_cursor")
tests = "".join(p.read_text(errors="replace") for p in (WORK / "tests").glob("test_*.py"))
c.ok(re.search(r"next_cursor|after", tests) is not None, "the project's tests cover paging")
with with_hidden() as d:
    code, out = run("python3 -m unittest discover -s tests -t . -v", cwd=d, timeout=60)
c.ok(code == 0, "the project's tests and the paging tests pass" if code == 0 else "tests fail:\n" + tail(out, 3500))
c.done()
