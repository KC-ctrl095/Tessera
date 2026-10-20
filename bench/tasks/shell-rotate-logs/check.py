import datetime
import gzip
import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path

from checklib import WORK, Check, run, tail

c = Check()
SCRIPT = WORK / "rotate_logs.sh"
# A zone far from UTC, so a script that uses local time names the archives wrongly.
ENV = {"TZ": "JST-9", "LANG": "C.UTF-8"}


def ts(y, mo, d, h=12, mi=0):
    return datetime.datetime(y, mo, d, h, mi, tzinfo=datetime.timezone.utc).timestamp()


def gz(data):
    return gzip.compress(data, mtime=0)


def interpreter():
    first = SCRIPT.read_text(encoding="utf-8", errors="replace").splitlines()[:1]
    m = re.match(r"#!\s*(\S+)(?:\s+(\S+))?", first[0]) if first else None
    if not m:
        return ["sh"]
    prog = os.path.basename(m.group(1))
    if prog == "env" and m.group(2):
        prog = m.group(2)
    return [prog] if prog in ("sh", "bash", "dash", "ksh", "zsh") else ["sh"]


class Fixture:
    """A log folder: files with given bytes and modification times."""

    def __init__(self, root, name):
        self.dir = Path(root) / name
        self.dir.mkdir(parents=True)

    def file(self, rel, data, when):
        p = self.dir / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
        os.utime(p, (when, when))
        return p

    def state(self):
        """{relative path: (bytes or None for folders, mtime_ns, inode)}."""
        out = {}
        for dirpath, dirnames, filenames in os.walk(self.dir):
            for name in dirnames + filenames:
                p = Path(dirpath) / name
                st = p.stat()
                rel = p.relative_to(self.dir).as_posix()
                out[rel] = (None if p.is_dir() else p.read_bytes(), st.st_mtime_ns, st.st_ino)
        return out


def rotate(args, cwd):
    return run(interpreter() + [str(SCRIPT)] + [str(a) for a in args], cwd=cwd, timeout=30, env=ENV)


def compare(label, fx, before, rotated, new_archives, deleted):
    """rotated: logs that must now be empty (same inode); new_archives: {name: content};
    deleted: archives that must be gone. Everything else must be exactly as before."""
    after = fx.state()
    problems = []
    for rel in sorted(before):
        data, mtime, ino = before[rel]
        if rel in deleted:
            if rel in after:
                problems.append(f"{rel} should have been deleted (older than the KEEP newest)")
            continue
        if rel not in after:
            problems.append(f"{rel} is gone")
            continue
        adata, amtime, aino = after[rel]
        if rel in rotated:
            if aino != ino:
                problems.append(f"{rel} was replaced by a new file; it must be truncated in place")
            elif adata != b"":
                problems.append(f"{rel} should be empty after rotation ({len(adata)} bytes left)")
        elif adata != data or (data is not None and amtime != mtime):
            problems.append(f"{rel} should not have been touched")
    for rel, content in new_archives.items():
        if rel not in after:
            problems.append(f"expected the archive {rel!r}")
            continue
        try:
            got = gzip.decompress(after[rel][0])
        except (OSError, EOFError) as e:
            problems.append(f"{rel} isn't a gzip file ({e})")
            continue
        if got != content:
            problems.append(f"{rel} should unpack to the log's old content")
    extra = sorted(set(after) - set(before) - set(new_archives))
    if extra:
        problems.append(f"unexpected new files: {extra}")
    c.ok(not problems, label if not problems else f"{label}:\n  " + "\n  ".join(problems))


if not SCRIPT.is_file():
    c.fail("rotate_logs.sh is missing")
    c.done()

tmp = Path(tempfile.mkdtemp(prefix="bench-rotate-"))
try:
    cwd = tmp / "elsewhere"
    cwd.mkdir()

    # A. The basics: names from the UTC date, contents, truncation in place,
    #    and everything that isn't a non-empty .log file left alone.
    fx = Fixture(tmp, "logs A")
    app = b"2026-03-05 20:29:58 started\n2026-03-05 20:30:00 ready\n"
    web = "GET / 200\nGET /café 404\nno newline at the end".encode()
    fx.file("app.log", app, ts(2026, 3, 5, 20, 30))  # 05:30 on the 6th in JST
    fx.file("web server.log", web, ts(2026, 3, 6, 23, 59))
    fx.file("empty.log", b"", ts(2026, 3, 1))
    fx.file("notes.txt", b"not a log\n", ts(2026, 2, 1))
    fx.file("app.log.bak", b"old copy\n", ts(2026, 1, 1))
    fx.file("sub/inner.log", b"in a subfolder\n", ts(2026, 3, 2))
    fx.file("stats.log/part1", b"a folder named like a log\n", ts(2026, 3, 2))
    os.utime(fx.dir / "sub", (ts(2026, 3, 2), ts(2026, 3, 2)))
    os.utime(fx.dir / "stats.log", (ts(2026, 3, 2), ts(2026, 3, 2)))
    before = fx.state()
    code, out = rotate([fx.dir, 5], cwd)
    c.ok(code == 0, "rotation exits 0" if code == 0 else f"rotation exited {code}:\n{tail(out)}")
    compare(
        "rotates non-empty logs into <name>.log.<UTC date>.gz and leaves the rest alone",
        fx, before, {"app.log", "web server.log"},
        {"app.log.20260305.gz": app, "web server.log.20260306.gz": web}, set(),
    )

    # B. An archive with that name exists already: the first free -N.
    fx = Fixture(tmp, "logs B")
    fx.file("app.log", b"newest lines\n", ts(2026, 4, 10, 9))
    fx.file("app.log.20260410.gz", gz(b"first\n"), ts(2026, 4, 10, 0, 5))
    fx.file("app.log.20260410-2.gz", gz(b"second\n"), ts(2026, 4, 10, 6))
    before = fx.state()
    code, out = rotate([fx.dir, 10], cwd)
    c.ok(code == 0, "rotation with existing archives exits 0" if code == 0 else f"rotation exited {code}:\n{tail(out)}")
    compare("a taken archive name gets the next free number (-3)", fx, before, {"app.log"},
            {"app.log.20260410-3.gz": b"newest lines\n"}, set())

    # C. Pruning by the date in the name (the files' times say the opposite),
    #    for rotated and for empty logs, with look-alike files that aren't archives.
    fx = Fixture(tmp, "logs C")
    fx.file("app.log", b"march\n", ts(2026, 3, 5, 10))
    old = ["20260101", "20260201", "20260215", "20260215-2", "20260301"]
    for i, d in enumerate(old):
        fx.file(f"app.log.{d}.gz", gz(d.encode()), ts(2026, 9, 1) - i * 86400)
    fx.file("app.log.2026.gz", gz(b"not an archive name"), ts(2025, 1, 1))
    fx.file("app.log.old.gz", gz(b"not an archive name"), ts(2025, 1, 1))
    fx.file("api.log", b"", ts(2026, 2, 1))
    for i, d in enumerate(["20260105", "20260110", "20260120", "20260130"]):
        fx.file(f"api.log.{d}.gz", gz(d.encode()), ts(2026, 9, 1) - i * 86400)
    fx.file("web server.log", b"web\n", ts(2026, 3, 4))
    for d in ["20260101", "20260102", "20260103"]:
        fx.file(f"web server.log.{d}.gz", gz(d.encode()), ts(2026, 3, 4))
    before = fx.state()
    code, out = rotate([fx.dir, 3], cwd)
    c.ok(code == 0, "rotation with pruning exits 0" if code == 0 else f"rotation exited {code}:\n{tail(out)}")
    compare(
        "keeps the KEEP newest archives of every log by the date in the name (-2 newer than none)",
        fx, before, {"app.log", "web server.log"},
        {"app.log.20260305.gz": b"march\n", "web server.log.20260304.gz": b"web\n"},
        {"app.log.20260101.gz", "app.log.20260201.gz", "app.log.20260215.gz",
         "api.log.20260105.gz", "web server.log.20260101.gz"},
    )

    # D. Numbers past 9 sort as numbers, and KEEP=2.
    fx = Fixture(tmp, "logs D")
    fx.file("app.log", b"june\n", ts(2026, 6, 1, 7))
    names = ["20260601"] + [f"20260601-{n}" for n in range(2, 11)]
    for d in names:
        fx.file(f"app.log.{d}.gz", gz(d.encode()), ts(2026, 6, 1))
    fx.file("app.log.20260531.gz", gz(b"may"), ts(2026, 6, 2))
    before = fx.state()
    code, out = rotate([fx.dir, 2], cwd)
    c.ok(code == 0, "rotation with many archives exits 0" if code == 0 else f"rotation exited {code}:\n{tail(out)}")
    compare(
        "-11 and -10 are the newest of the same day",
        fx, before, {"app.log"}, {"app.log.20260601-11.gz": b"june\n"},
        {f"app.log.{d}.gz" for d in names if d != "20260601-10"} | {"app.log.20260531.gz"},
    )

    # E. Wrong usage: a usage line on stderr, exit 2, nothing changed.
    fx = Fixture(tmp, "logs E")
    fx.file("app.log", b"keep me\n", ts(2026, 5, 5))
    fx.file("app.log.20260101.gz", gz(b"a"), ts(2026, 1, 1))
    fx.file("app.log.20260102.gz", gz(b"b"), ts(2026, 1, 2))
    afile = tmp / "plain-file"
    afile.write_text("x")
    before = fx.state()
    cases = [
        ([], "no arguments"),
        ([fx.dir], "no KEEP"),
        ([fx.dir, 0], "KEEP 0"),
        ([fx.dir, -1], "KEEP -1"),
        ([fx.dir, "abc"], "KEEP abc"),
        ([fx.dir, "1.5"], "KEEP 1.5"),
        ([fx.dir, ""], "an empty KEEP"),
        ([tmp / "no such dir", 3], "a missing DIR"),
        ([afile, 3], "DIR is a file"),
    ]
    bad = []
    env = dict(os.environ, **ENV)
    env.pop("PYTHONPATH", None)
    for args, what in cases:
        cmd = interpreter() + [str(SCRIPT)] + [str(a) for a in args]
        try:
            p = subprocess.run(cmd, cwd=str(cwd), env=env, capture_output=True, timeout=30,
                               start_new_session=True)
        except subprocess.TimeoutExpired:
            bad.append(f"{what}: timed out")
            continue
        if p.returncode != 2:
            bad.append(f"{what}: exit {p.returncode}, expected 2")
        elif b"usage" not in p.stderr.lower():
            bad.append(f"{what}: no usage line on stderr")
    c.ok(not bad, "wrong usage prints a usage line to stderr and exits 2" if not bad else "wrong usage:\n  " + "\n  ".join(bad))
    after = fx.state()
    c.ok(after == before, "wrong usage changes nothing")
finally:
    shutil.rmtree(tmp, ignore_errors=True)
c.done()
