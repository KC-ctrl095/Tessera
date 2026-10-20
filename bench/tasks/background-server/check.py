import json
import os
import signal
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

from checklib import FILES, HIDDEN, WORK, Check, copy_files, run, tail, unchanged, with_hidden

c = Check()
if not unchanged("tests/integration.py"):
    c.note("tests/integration.py was changed; the original is used")
pid_left = WORK / "run" / "parcelhub.pid"
if pid_left.exists():
    c.note("run/parcelhub.pid is still there: the dev server wasn't stopped (or was killed without a clean shutdown)")


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def healthy(base):
    try:
        with urllib.request.urlopen(base + "/health", timeout=2) as r:
            return r.status == 200
    except OSError:
        return False


with with_hidden(overlay=HIDDEN / "none") as d:
    # The original tests and data; the agent's service, on a fresh port.
    copy_files(FILES / "tests", d / "tests")
    copy_files(FILES / "data", d / "data")
    for stale in (d / "run").glob("*"):
        stale.unlink()
    code, out = run([sys.executable, str(HIDDEN / "unit_checks.py")], cwd=d, timeout=30)
    try:
        unit = json.loads(out.strip().splitlines()[-1])
    except (ValueError, IndexError):
        unit = {"failures": [f"unit checks crashed:\n{tail(out)}"], "skipped": []}
    for f in unit["failures"]:
        # Where the URL gets encoded is the agent's choice (the HTTP checks
        # test the links); Page.has_next itself must be right.
        if f.startswith("build_url"):
            c.note(f"unit check (not counted): {f}")
        else:
            c.fail(f)
    for s in unit["skipped"]:
        c.note(f"unit check skipped (function reshaped): {s}")

    port = free_port()
    cfg = json.loads((FILES / "config" / "dev.json.example").read_text())
    cfg["port"] = port
    (d / "config").mkdir(exist_ok=True)
    (d / "config" / "dev.json").write_text(json.dumps(cfg, indent=2) + "\n")
    base = f"http://127.0.0.1:{port}"
    log = open(d / "server-under-check.log", "w+")
    env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
    env.pop("PYTHONPATH", None)
    env.pop("PARCELHUB_CONFIG", None)
    server = subprocess.Popen([sys.executable, "-m", "parcelhub.serve"], cwd=d, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    try:
        t0 = time.time()
        while time.time() - t0 < 75 and server.poll() is None and not healthy(base):
            time.sleep(0.5)
        up = healthy(base)
        log.seek(0)
        if c.ok(up, f"the service came up in {time.time() - t0:.0f} s" if up else "the service didn't come up within 75 s:\n" + tail(log.read(), 1500)):
            code, out = run([sys.executable, "tests/integration.py"], cwd=d, timeout=60)
            c.ok(code == 0 and "8/8 checks passed" in out,
                 "the 8 integration checks pass" if code == 0 else "integration checks fail:\n" + tail(out, 1500))
            code, out = run([sys.executable, str(HIDDEN / "extra_http.py"), base, str(FILES / "data" / "parcels.json")], cwd=d, timeout=60)
            c.ok(code == 0, "extra paging and encoding checks pass" if code == 0 else "extra checks fail:\n" + tail(out, 1800))
    finally:
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(server.pid, sig)
                server.wait(timeout=5)
                break
            except ProcessLookupError:
                break
            except subprocess.TimeoutExpired:
                continue
        log.close()
c.done()
