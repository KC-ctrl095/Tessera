"""Helpers for task checks.

A check runs with the work dir as its current directory and these variables:
BENCH_WORK (the work dir), BENCH_TASK (the task folder, with `files/` and the
hidden assets), BENCH_ANSWER (a file holding the agent's final answer; may be
empty). It prints what it found and exits 0 on a pass, 1 on a failure.

Checks never trust the work dir's own tests: `with_hidden()` gives a scratch
copy of the work dir with the task's hidden files laid over it.
"""

import contextlib
import difflib
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

WORK = Path(os.environ.get("BENCH_WORK", os.getcwd())).resolve()
TASK = Path(os.environ.get("BENCH_TASK", ".")).resolve()
FILES = TASK / "files"
HIDDEN = TASK / "hidden"

# Never copied into scratch dirs (big, generated, or the check's own business).
SKIP_DIRS = {".git", "target", "node_modules", "__pycache__", ".pytest_cache", ".mypy_cache"}


def answer():
    """The agent's final answer text ("" when there was none)."""
    p = os.environ.get("BENCH_ANSWER")
    if not p or not os.path.exists(p):
        return ""
    return Path(p).read_text(encoding="utf-8", errors="replace")


def stored_name(rel):
    """The name a work-dir path has under `files/` (dotfiles are stored as `dot_x`)."""
    parts = []
    for part in Path(rel).parts:
        parts.append("dot_" + part[1:] if part.startswith(".") else part)
    return Path(*parts)


def original_bytes(rel):
    """The file as the task handed it out (None when it didn't exist)."""
    p = FILES / stored_name(rel)
    return p.read_bytes() if p.is_file() else None


def original_text(rel):
    b = original_bytes(rel)
    return None if b is None else b.decode("utf-8")


def text(rel):
    p = WORK / rel
    return p.read_text(encoding="utf-8", errors="replace") if p.is_file() else None


def unchanged(rel):
    """True when the work-dir file is byte-identical to the handed-out one."""
    p = WORK / rel
    orig = original_bytes(rel)
    if orig is None:
        return not p.exists()
    return p.is_file() and p.read_bytes() == orig


def tree_digest(root, skip=SKIP_DIRS):
    """A digest of every file (path, mode bits for x, bytes) under root."""
    root = Path(root)
    h = hashlib.sha256()
    if not root.exists():
        return None
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in skip)
        for name in sorted(filenames):
            p = Path(dirpath) / name
            rel = p.relative_to(root).as_posix()
            h.update(rel.encode() + b"\0")
            if p.is_symlink():
                h.update(b"L" + os.readlink(p).encode())
            else:
                h.update(p.read_bytes())
            h.update(b"\0")
    return h.hexdigest()


def original_tree_digest(rel):
    """`tree_digest` of a folder as handed out (dot_ names mapped back)."""
    src = FILES / stored_name(rel)
    with tempfile.TemporaryDirectory(prefix="bench-orig-") as tmp:
        dst = Path(tmp) / "t"
        copy_files(src, dst)
        return tree_digest(dst)


def copy_files(src, dst):
    """Copy a `files/` tree, turning `dot_name` into `.name` (as the runner does)."""
    src, dst = Path(src), Path(dst)
    if src.is_file():
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dst)
        return
    for dirpath, dirnames, filenames in os.walk(src):
        rel = Path(dirpath).relative_to(src)
        out_dir = dst / Path(*[_undot(p) for p in rel.parts]) if rel.parts else dst
        out_dir.mkdir(parents=True, exist_ok=True)
        shutil.copystat(dirpath, out_dir)
        for name in filenames:
            shutil.copy2(Path(dirpath) / name, out_dir / _undot(name))


def _undot(name):
    return "." + name[4:] if name.startswith("dot_") and len(name) > 4 else name


def run(cmd, cwd=None, timeout=120, env=None, input=None):
    """Run a command; returns (exit code, combined output). Timeouts give 124."""
    full_env = dict(os.environ)
    # Python must not write bytecode into the agent's tree, and tests must
    # not pick up the check's own helpers.
    full_env["PYTHONDONTWRITEBYTECODE"] = "1"
    full_env.pop("PYTHONPATH", None)
    if env:
        full_env.update(env)
    try:
        p = subprocess.run(
            cmd,
            cwd=str(cwd or WORK),
            shell=isinstance(cmd, str),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=timeout,
            env=full_env,
            input=input,
            text=True,
            errors="replace",
            start_new_session=True,
        )
        return p.returncode, p.stdout
    except subprocess.TimeoutExpired as e:
        out = e.stdout or ""
        if isinstance(out, bytes):
            out = out.decode("utf-8", "replace")
        return 124, out + f"\n[timed out after {timeout}s]"


@contextlib.contextmanager
def with_hidden(overlay=None, keep_git=False):
    """A scratch copy of the work dir with the hidden files laid over it."""
    overlay = HIDDEN if overlay is None else Path(overlay)
    tmp = Path(tempfile.mkdtemp(prefix="bench-check-"))
    dst = tmp / "w"
    skip = SKIP_DIRS - ({".git"} if keep_git else set())
    shutil.copytree(WORK, dst, symlinks=True, ignore=lambda d, names: [n for n in names if n in skip])
    if overlay.exists():
        copy_files(overlay, dst)
    try:
        yield dst
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def git(*args, cwd=None):
    """Run git in the work dir; returns (code, stdout stripped)."""
    p = subprocess.run(
        ["git", *args],
        cwd=str(cwd or WORK),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
    )
    return p.returncode, p.stdout.strip()


def diff_lines(rel):
    """(removed, added) lines of the work file against the handed-out one."""
    a = (original_text(rel) or "").splitlines()
    b = (text(rel) or "").splitlines()
    removed, added = [], []
    for line in difflib.ndiff(a, b):
        if line.startswith("- "):
            removed.append(line[2:])
        elif line.startswith("+ "):
            added.append(line[2:])
    return removed, added


def changed_regions(rel):
    """Line ranges (1-based, in the original) that differ: [(start, end), …]."""
    a = (original_text(rel) or "").splitlines()
    b = (text(rel) or "").splitlines()
    out = []
    for tag, i1, i2, _j1, _j2 in difflib.SequenceMatcher(None, a, b, autojunk=False).get_opcodes():
        if tag != "equal":
            out.append((i1 + 1, max(i1 + 1, i2)))
    return out


def load_json(rel):
    p = WORK / rel
    with open(p, encoding="utf-8") as f:
        return json.load(f)


class Check:
    """Collects findings; `done()` prints them and exits."""

    def __init__(self):
        self.failures = []
        self.notes = []

    def ok(self, cond, what):
        (self.notes if cond else self.failures).append(what)
        return bool(cond)

    def fail(self, what):
        self.failures.append(what)

    def note(self, what):
        self.notes.append(what)

    def done(self):
        for n in self.notes:
            print(f"ok: {n}")
        for f in self.failures:
            print(f"FAIL: {f}")
        print("PASS" if not self.failures else "FAIL")
        sys.exit(0 if not self.failures else 1)


def tail(s, n=1500):
    s = s or ""
    return s if len(s) <= n else "…" + s[-n:]
