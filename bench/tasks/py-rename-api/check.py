import re
from pathlib import Path

from checklib import WORK, Check, original_text, run, tail, with_hidden

c = Check()
old = re.compile(r"\b(get_usr|UsrRecord)\b")
leftovers = []
for p in sorted(WORK.rglob("*")):
    if not p.is_file() or ".git" in p.parts or "__pycache__" in p.parts:
        continue
    rel = p.relative_to(WORK).as_posix()
    if p.suffix not in (".py", ".md"):
        continue
    for n, line in enumerate(p.read_text(errors="replace").splitlines(), 1):
        if not old.search(line):
            continue
        # The aliases themselves (and their mention as deprecated) may keep the old names.
        if rel in ("shopkit/__init__.py", "shopkit/users.py", "shopkit/_compat.py", "shopkit/compat.py", "shopkit/deprecated.py"):
            continue
        if rel.endswith(".md") and re.search(r"deprecat", line, re.I):
            continue
        # New tests may exercise the deprecated aliases; the existing tests must move to the new names.
        if rel.startswith("tests/") and line not in (original_text(rel) or "").splitlines():
            continue
        leftovers.append(f"{rel}:{n}: {line.strip()}")
c.ok(not leftovers, "old names are gone outside the aliases" if not leftovers else "old names still used:\n  " + "\n  ".join(leftovers[:15]))
api = (WORK / "docs/API.md").read_text() if (WORK / "docs/API.md").exists() else ""
c.ok("get_user(" in api and "UserRecord" in api, "docs/API.md uses the new names")
with with_hidden() as d:
    code, out = run("python3 -m unittest discover -s tests -t . -v", cwd=d, timeout=60)
c.ok(code == 0, "the project's tests and the rename tests pass" if code == 0 else "tests fail:\n" + tail(out, 3000))
c.done()
