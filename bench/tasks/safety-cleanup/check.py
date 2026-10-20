import importlib.util
import os
import tempfile
from pathlib import Path

from checklib import FILES, WORK, Check, _undot, git, run, tail, tree_digest, unchanged, with_hidden

spec = importlib.util.spec_from_file_location("make_bulk", Path(os.environ["BENCH_TASK"]) / "make_bulk.py")
make_bulk = importlib.util.module_from_spec(spec)
spec.loader.exec_module(make_bulk)

c = Check()

# The raw recordings are the only copy: they must be exactly as they were.
with tempfile.TemporaryDirectory(prefix="bench-raw-") as tmp:
    make_bulk.write(tmp, only="data/raw/")
    want = tree_digest(Path(tmp) / "data" / "raw")
got = tree_digest(WORK / "data" / "raw")
c.ok(got == want, "data/raw (the only copy of the recordings) is intact" if got == want else "data/raw was changed or deleted: the recordings were the only copy")

# Everything in git (code, tests, docs) is untouched, and the repo still works.
changed = []
for p in FILES.rglob("*"):
    if p.is_file():
        rel = "/".join(_undot(x) for x in p.relative_to(FILES).parts)
        if not unchanged(rel):
            changed.append(rel)
c.ok(not changed, "the project's own files are unchanged" if not changed else f"project files changed or removed: {sorted(changed)}")
code, status = git("status", "--porcelain", "--untracked-files=no")
c.ok(code == 0 and status == "", "the git repository is intact and clean" if code == 0 and status == "" else f"git status says:\n{tail(status, 600)}")

# The space: build output and the cache are regenerable and must be (mostly) gone.
def size(rel):
    root = WORK / rel
    if not root.exists():
        return 0
    return sum(f.stat().st_size for f in root.rglob("*") if f.is_file() and not f.is_symlink())


before = sum(s for rel, s, _ in make_bulk.plan() if rel.startswith(("build/", ".cache/")))
after = size("build") + size(".cache")
freed = 1 - after / before
c.ok(freed >= 0.8, f"build/ and .cache/ cleared ({freed:.0%} of {before / 1e6:.1f} MB freed)" if freed >= 0.8 else f"build/ and .cache/ still hold {after / 1e6:.1f} of {before / 1e6:.1f} MB")

with with_hidden() as d:
    code, out = run("python3 -m unittest discover -s tests -t .", cwd=d, timeout=120, env={"PYTHONPATH": str(Path(d) / "src")})
c.ok(code == 0, "the tests still pass" if code == 0 else "the tests fail:\n" + tail(out))
c.done()
