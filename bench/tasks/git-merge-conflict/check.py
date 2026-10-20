import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path

from checklib import TASK, WORK, Check, git, original_tree_digest, run, tail, tree_digest

FEATURE = "feature/retries"

c = Check()


def show(spec):
    return subprocess.run(["git", "show", spec], cwd=WORK, capture_output=True, text=True).stdout


def expected_tips():
    """Replay setup.sh on the work repo's own root commit: main and feature as handed out."""
    code, roots = git("rev-list", "--max-parents=0", "main")
    roots = roots.split()
    if code != 0 or len(roots) != 1:
        return None, f"main should have one root commit, found {roots}"
    tmp = Path(tempfile.mkdtemp(prefix="bench-replay-"))
    try:
        r = tmp / "r"
        subprocess.run(["git", "clone", "-q", "--no-checkout", str(WORK), str(r)], check=True, capture_output=True)
        subprocess.run(["git", "config", "commit.gpgsign", "false"], cwd=r, check=True)
        subprocess.run(["git", "checkout", "-q", "-f", "-B", "main", roots[0]], cwd=r, check=True, capture_output=True)
        if tree_digest(r) != original_tree_digest("."):
            return None, "the initial commit was rewritten"
        env = dict(os.environ, BENCH_TASK=str(TASK), BENCH_WORK=str(r))
        subprocess.run(["sh", str(TASK / "setup.sh")], cwd=r, env=env, check=True, capture_output=True)
        main = subprocess.run(["git", "rev-parse", "main"], cwd=r, capture_output=True, text=True).stdout.strip()
        feat = subprocess.run(["git", "rev-parse", FEATURE], cwd=r, capture_output=True, text=True).stdout.strip()
        return (main, feat), None
    except subprocess.CalledProcessError as e:
        return None, f"could not replay the setup: {e}"
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


tips, why = expected_tips()
if not c.ok(tips is not None, "history as handed out found" if tips else why):
    c.done()
old_main, feature_tip = tips

code, head_ref = git("symbolic-ref", "-q", "HEAD")
c.ok(head_ref == "refs/heads/main", "HEAD is on main" if head_ref == "refs/heads/main" else f"HEAD is {head_ref or 'detached'}, not main")

code, feat_now = git("rev-parse", "-q", "--verify", f"refs/heads/{FEATURE}")
c.ok(feat_now == feature_tip, f"{FEATURE} is unchanged" if feat_now == feature_tip else f"{FEATURE} was changed or deleted")

code, line = git("rev-list", "--parents", "-n", "1", "main")
parents = line.split()[1:]
c.ok(
    parents == [old_main, feature_tip],
    "main's tip is a merge of the old main and the feature branch"
    if parents == [old_main, feature_tip]
    else f"main's tip is not a merge commit of the old main and {FEATURE} (parents: {parents})",
)

code, markers = git("grep", "-n", "-E", r"^(<<<<<<<|>>>>>>>)( |$)|^=======$", "main", "--")
c.ok(code != 0, "no conflict markers left" if code != 0 else "conflict markers left:\n" + markers)

ns = {}
try:
    exec(compile(show("main:config.py"), "config.py", "exec"), ns)
except Exception as e:  # a broken config is a failed merge
    c.fail(f"config.py on main doesn't run: {e}")
want = {
    "TIMEOUT": 30,
    "RETRIES": 5,
    "BACKOFF": 1.5,
    "API_URL": "https://helpdesk.example.invalid/api/v2",
    "USER_AGENT": "ticket-sync/1.2",
    "PAGE_SIZE": 50,
    "SYNC_EVERY": 300,
}
got = {k: ns.get(k) for k in want}
c.ok(got == want, "config.py has main's TIMEOUT and the feature's retry settings" if got == want else f"config.py values: {got}, expected {want}")

log = show("main:CHANGELOG.md")
lines = [line.strip() for line in log.splitlines()]


def find(pred):
    for i, line in enumerate(lines):
        if pred(line):
            return i
    return -1


def count(pred):
    return sum(1 for line in lines if pred(line))


order = [
    find(lambda s: re.fullmatch(r"##\s*2026-09-04", s) is not None),
    find(lambda s: s.startswith("-") and "retried up to 5 times" in s),
    find(lambda s: re.fullmatch(r"##\s*2026-09-02", s) is not None),
    find(lambda s: s.startswith("-") and "timeout to 30 seconds" in s),
    find(lambda s: re.fullmatch(r"##\s*2026-08-20", s) is not None),
    find(lambda s: s.startswith("- First release")),
]
once = all(
    count(p) == 1
    for p in (
        lambda s: "retried up to 5 times" in s,
        lambda s: "timeout to 30 seconds" in s,
        lambda s: s.startswith("- First release"),
    )
)
ordered = -1 not in order and order == sorted(order) and once
c.ok(
    ordered,
    "CHANGELOG.md has both new entries, newest (2026-09-04) first"
    if ordered
    else "CHANGELOG.md should list 2026-09-04 (retries), then 2026-09-02 (timeout), then 2026-08-20, each once:\n" + tail(log, 1200),
)

# The tests run on main's committed files, with the original tests laid over them.
with tempfile.TemporaryDirectory(prefix="bench-merged-") as tmp:
    arch = subprocess.run(["git", "archive", "main"], cwd=WORK, capture_output=True)
    subprocess.run(["tar", "-x", "-C", tmp], input=arch.stdout, check=False)
    for name in ("test_sync.py", "test_dedupe.py"):
        shutil.copy2(TASK / "hidden" / name, Path(tmp) / name)
    code, out = run("python3 -m unittest -v test_sync test_dedupe", cwd=tmp, timeout=60)
c.ok(code == 0, "the tests pass on the merged main" if code == 0 else "tests fail on the merged main:\n" + tail(out))

code, status = git("status", "--porcelain", "--untracked-files=no")
code2, _ = git("rev-parse", "-q", "--verify", "MERGE_HEAD")
clean = status == "" and code2 != 0
c.ok(clean, "nothing left uncommitted" if clean else "the merge isn't finished or changes are uncommitted:\n" + status)
c.done()
