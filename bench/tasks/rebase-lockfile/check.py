import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path

from checklib import FILES, HIDDEN, TASK, WORK, Check, git, original_tree_digest, run, tail, tree_digest

FEATURE = "feature/export"
c = Check()


def replay():
    """setup.sh replayed on the work repo's own root commit: main, the feature tip, the feature's messages."""
    code, roots = git("rev-list", "--max-parents=0", "--all")
    roots = sorted(set(roots.split()))
    if code != 0 or len(roots) != 1:
        return None, f"expected one root commit, found {roots}"
    tmp = Path(tempfile.mkdtemp(prefix="bench-replay-"))
    try:
        r = tmp / "r"
        subprocess.run(["git", "clone", "-q", "--no-checkout", str(WORK), str(r)], check=True, capture_output=True)
        subprocess.run(["git", "config", "commit.gpgsign", "false"], cwd=r, check=True)
        subprocess.run(["git", "checkout", "-q", "-f", "-B", "main", roots[0]], cwd=r, check=True, capture_output=True)
        subprocess.run(["git", "branch", "-q", "-D", FEATURE], cwd=r, capture_output=True)
        if tree_digest(r) != original_tree_digest("."):
            return None, "the initial commit was rewritten"
        env = dict(os.environ, BENCH_TASK=str(TASK), BENCH_WORK=str(r))
        subprocess.run(["sh", str(TASK / "setup.sh")], cwd=r, env=env, check=True, capture_output=True)

        def out(*a):
            return subprocess.run(["git", *a], cwd=r, capture_output=True, text=True).stdout

        main = out("rev-parse", "main").strip()
        feat = out("rev-parse", FEATURE).strip()
        msgs = [out("log", "-1", "--format=%B", h).strip() for h in out("rev-list", "--reverse", f"main..{FEATURE}").split()]
        return (main, feat, msgs), None
    except subprocess.CalledProcessError as e:
        return None, f"could not replay the setup: {e}"
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def show(spec):
    p = subprocess.run(["git", "show", spec], cwd=WORK, capture_output=True)
    return p.stdout if p.returncode == 0 else None


def requirements(text):
    out = {}
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if line:
            m = re.match(r"([A-Za-z0-9._-]+)(.*)", line)
            out[m.group(1).lower()] = re.sub(r"\s+", "", m.group(2))
    return out


def generated_lock(req, index):
    with tempfile.TemporaryDirectory(prefix="bench-lock-") as tmp:
        t = Path(tmp)
        (t / "tools").mkdir()
        shutil.copy2(FILES / "tools" / "lock.py", t / "tools" / "lock.py")
        (t / "tools" / "index.json").write_bytes(index)
        (t / "requirements.in").write_bytes(req)
        p = subprocess.run(["python3", "tools/lock.py"], cwd=t, capture_output=True, text=True)
        if p.returncode != 0:
            return None, p.stderr.strip()
        return (t / "deps.lock").read_bytes(), None


got, why = replay()
if not c.ok(got is not None, "history as handed out found" if got else why):
    c.done()
old_main, old_feature, messages = got

code, main_now = git("rev-parse", "-q", "--verify", "refs/heads/main")
c.ok(main_now == old_main, "main is unchanged" if main_now == old_main else "main was changed (only feature/export should move)")
code, tip = git("rev-parse", "-q", "--verify", f"refs/heads/{FEATURE}")
if not c.ok(code == 0, f"{FEATURE} exists" if code == 0 else f"{FEATURE} is gone"):
    c.done()
c.ok(tip != old_feature, f"{FEATURE} was rebased" if tip != old_feature else f"{FEATURE} is where it was (not rebased)")
code, _ = git("merge-base", "--is-ancestor", old_main, tip)
c.ok(code == 0, "main's tip is an ancestor of the branch" if code == 0 else f"{FEATURE} doesn't contain main's tip")
_, merges = git("rev-list", "--merges", f"{old_main}..{tip}")
c.ok(not merges, "no merge commits on the branch" if not merges else "the branch has merge commits (rebase, don't merge)")
_, revs = git("rev-list", "--reverse", f"{old_main}..{tip}")
commits = revs.split()
got_msgs = [git("log", "-1", "--format=%B", h)[1].strip() for h in commits]
c.ok(got_msgs == messages, "the branch's three commits, same messages, in order" if got_msgs == messages
     else f"expected the 3 feature commits on top of main ({[m.splitlines()[0] for m in messages]}), found {[m.splitlines()[0] if m else '' for m in got_msgs]}")

main_req = requirements(show(f"{old_main}:requirements.in").decode())
main_index = show(f"{old_main}:tools/index.json")
want_req = [dict(main_req, tabwrite=">=1.2"), dict(main_req, tabwrite=">=1.2"), dict(main_req, tabwrite=">=1.2,<2")]
for i, h in enumerate(commits[:3]):
    label = f"commit {i + 1} ({got_msgs[i].splitlines()[0] if got_msgs[i] else h[:8]})"
    req, index, lock = show(f"{h}:requirements.in"), show(f"{h}:tools/index.json"), show(f"{h}:deps.lock")
    if req is None or index is None or lock is None:
        c.fail(f"{label}: requirements.in, tools/index.json or deps.lock is missing")
        continue
    try:
        r = requirements(req.decode())
    except (AttributeError, UnicodeDecodeError):
        r = None
    c.ok(r == want_req[i], f"{label}: requirements.in has both sides' pins" if r == want_req[i] else f"{label}: requirements.in is {r}, expected {want_req[i]}")
    c.ok(index == main_index, f"{label}: tools/index.json is main's" if index == main_index else f"{label}: tools/index.json differs from main's")
    want_lock, err = generated_lock(req, index)
    if want_lock is None:
        c.fail(f"{label}: tools/lock.py can't lock its requirements.in: {err}")
    else:
        c.ok(lock == want_lock, f"{label}: deps.lock matches tools/lock.py's output" if lock == want_lock
             else f"{label}: deps.lock is not what tools/lock.py generates for this commit's requirements.in and index")
    code, markers = git("grep", "-n", "-E", r"^(<<<<<<<|>>>>>>>)( |$)|^=======$", h, "--")
    c.ok(code != 0, f"{label}: no conflict markers" if code != 0 else f"{label}: conflict markers left:\n" + tail(markers, 600))

# The tests at the tip: the branch's own, and the hidden ones (both sides' behaviour).
with tempfile.TemporaryDirectory(prefix="bench-tip-") as tmp:
    arch = subprocess.run(["git", "archive", tip], cwd=WORK, capture_output=True)
    subprocess.run(["tar", "-x", "-C", tmp], input=arch.stdout, check=False)
    code, out = run("python3 -m unittest discover -s tests -t .", cwd=tmp, timeout=60)
    c.ok(code == 0, "the branch's own tests pass at its tip" if code == 0 else "the branch's own tests fail at its tip:\n" + tail(out))
    shutil.rmtree(Path(tmp) / "tests", ignore_errors=True)
    shutil.copytree(HIDDEN / "tests", Path(tmp) / "tests")
    code, out = run("python3 -m unittest discover -s tests -t .", cwd=tmp, timeout=60)
    c.ok(code == 0, "main's and the feature's behaviour both work at the tip" if code == 0 else "at the tip, main's or the feature's behaviour is lost:\n" + tail(out))

code, status = git("status", "--porcelain", "--untracked-files=no")
busy = [p for p in ("rebase-merge", "rebase-apply", "MERGE_HEAD", "CHERRY_PICK_HEAD") if (WORK / ".git" / p).exists()]
c.ok(status == "" and not busy, "the rebase is finished and the tree is clean" if status == "" and not busy
     else f"unfinished: {busy or ''} {status}")
c.done()
