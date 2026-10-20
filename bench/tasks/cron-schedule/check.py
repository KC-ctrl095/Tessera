import os
import re
import shlex

from checklib import Check, text

c = Check()
REL = "deploy/crontab"
DIR = "/opt/shop/scripts/"
LOGS = "/var/log/shop/"

MONTHS = {m: i + 1 for i, m in enumerate("jan feb mar apr may jun jul aug sep oct nov dec".split())}
DAYS = {d: i for i, d in enumerate("sun mon tue wed thu fri sat".split())}
RANGES = [(0, 59, {}), (0, 23, {}), (1, 31, {}), (1, 12, MONTHS), (0, 7, DAYS)]
MACROS = {
    "@yearly": "0 0 1 1 *", "@annually": "0 0 1 1 *", "@monthly": "0 0 1 * *",
    "@weekly": "0 0 * * 0", "@daily": "0 0 * * *", "@midnight": "0 0 * * *", "@hourly": "0 * * * *",
}


def expand(field, lo, hi, names):
    """The set of values a cron field matches (raises ValueError when it isn't valid)."""
    def val(s):
        s = s.lower()
        if s in names:
            return names[s]
        if not s.isdigit():
            raise ValueError(f"bad value {s!r}")
        return int(s)

    out = set()
    for part in field.split(","):
        step = None
        if "/" in part:
            part, st = part.split("/", 1)
            if not st.isdigit() or int(st) < 1:
                raise ValueError(f"bad step in {field!r}")
            step = int(st)
        if part == "*":
            a, b = lo, hi
        elif "-" in part:
            x, y = part.split("-", 1)
            a, b = val(x), val(y)
        else:
            a = val(part)
            b = hi if step else a
        if not (lo <= a <= b <= hi):
            raise ValueError(f"out of range in {field!r}")
        out |= set(range(a, b + 1, step or 1))
    return out


def schedule(fields):
    """(minutes, hours, months, days) where days is the set of (day of month, weekday) pairs that run."""
    sets = [expand(f, *RANGES[i]) for i, f in enumerate(fields)]
    dow = {0 if d == 7 else d for d in sets[4]}
    dom_star, dow_star = fields[2].startswith("*"), fields[4].startswith("*")
    days = set()
    for dom in range(1, 32):
        for wd in range(7):
            a, b = dom in sets[2], wd in dow
            # Vixie cron: when both day fields are restricted, either may match.
            if ((a and b) if (dom_star or dow_star) else (a or b)):
                days.add((dom, wd))
    return sets[0], sets[1], sets[3], days


def describe_diff(name, got, want):
    labels = ["minutes", "hours", "months", "days"]
    bad = [labels[i] for i in range(4) if got[i] != want[i]]
    return f"{name} runs at the wrong times (wrong {', '.join(bad)})"


EXPECTED = {
    "backup.sh": "30 2 * * *",
    "cleanup.sh": "0 4 * * 0",
    "report.py": "15 18 * * 1-5",
    "healthcheck.sh": "*/5 * * * *",
}
WRAPPERS = {
    "cd", "env", "nice", "ionice", "flock", "timeout", "chronic", "cronic", "nohup", "exec",
    "sh", "bash", "dash", "python3", "test", "[", "run-one", "setlock",
}


def redirects(cmd):
    """Follow the redirections left to right: where stdout and stderr end up."""
    fds = {1: ("tty", None), 2: ("tty", None)}
    bash_only = False
    for m in re.finditer(r"(\d|&)?(>>|>)(&(\d)|\s*(\"[^\"]*\"|'[^']*'|[^\s;&|<>]+))", cmd):
        who, op, dup_fd, target = m.group(1), m.group(2), m.group(4), m.group(5)
        target = target.strip("\"'") if target else target
        if who == "&":
            bash_only = True
            fds[1] = fds[2] = (target, op)
        elif dup_fd:
            fds[int(who or 1)] = fds.get(int(dup_fd), ("?", None))
        else:
            fds[int(who or 1)] = (target, op)
    return fds, bash_only


raw = text(REL)
if raw is None:
    c.fail(f"{REL} is missing")
    c.done()

shell_is_bash = False
jobs = []
for n, line in enumerate(raw.splitlines(), 1):
    s = line.strip()
    if not s or s.startswith("#"):
        continue
    m = re.match(r"([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$", s)
    if m:
        if m.group(1) == "SHELL" and os.path.basename(m.group(2).strip("\"'")) == "bash":
            shell_is_bash = True
        continue
    if s.startswith("@"):
        parts = s.split(None, 1)
        if parts[0] not in MACROS or len(parts) < 2:
            c.fail(f"line {n}: not a schedule cron understands here: {s}")
            continue
        fields, cmd = MACROS[parts[0]].split(), parts[1]
    else:
        parts = s.split(None, 5)
        if len(parts) < 6:
            c.fail(f"line {n}: not a cron line: {s}")
            continue
        fields, cmd = parts[:5], parts[5]
    jobs.append((n, fields, cmd))

c.ok(raw.endswith("\n"), "the file ends with a newline (cron ignores a last line without one)")
c.ok(len(jobs) == 4, f"four jobs (found {len(jobs)})")

for script, when in EXPECTED.items():
    name = script.rsplit(".", 1)[0]
    mine = [j for j in jobs if re.search(r"(^|[/\s'\"])" + re.escape(script) + r"($|[\s;&|<>'\"])", j[2])]
    if len(mine) != 1:
        c.fail(f"{script}: expected one job, found {len(mine)}")
        continue
    n, fields, cmd = mine[0]
    try:
        got = schedule(fields)
    except ValueError as e:
        c.fail(f"{script} (line {n}): the time fields aren't valid: {e}")
        continue
    want = schedule(when.split())
    c.ok(got == want, f"{script} runs at the right times" if got == want else describe_diff(script, got, want))

    if re.search(r"(?<!\\)%", cmd):
        c.fail(f"{script} (line {n}): an unescaped % in a crontab command ends the command there")
    try:
        toks = shlex.split(cmd)
    except ValueError as e:
        c.fail(f"{script} (line {n}): the command doesn't parse: {e}")
        continue
    first = toks[0] if toks else ""
    plain_word = re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.-]*", first) is not None
    if plain_word and first not in WRAPPERS and "=" not in first:
        c.fail(f"{script} (line {n}): the command starts with {first!r}; a user crontab has no user column")
    # The script itself, by its full path (or relative to a `cd` before it).
    cwd, path_ok, idx = None, False, None
    for i, t in enumerate(toks):
        if i > 0 and toks[i - 1] == "cd":
            cwd = t
        full = t if t.startswith("/") or cwd is None else os.path.normpath(os.path.join(cwd, t))
        if full == DIR + script:
            path_ok, idx = True, i
            break
    c.ok(path_ok, f"{script} is run from {DIR}")
    if script.endswith(".py") and idx is not None:
        j = idx - 1
        while j >= 0 and toks[j].startswith("-"):
            j -= 1
        py = j >= 0 and re.fullmatch(r"python3(\.\d+)?", os.path.basename(toks[j])) is not None
        c.ok(py, f"{script} is run with python3")

    fds, bash_only = redirects(cmd)
    log = LOGS + name + ".log"
    for fd, label in ((1, "stdout"), (2, "stderr")):
        where, op = fds[fd]
        if (where, op) == (log, ">>"):
            c.note(f"{script}: {label} is appended to {log}")
        elif where == log:
            c.fail(f"{script}: {label} overwrites {log} instead of appending to it")
        else:
            c.fail(f"{script}: {label} should be appended to {log} (goes to {'the cron mail' if where == 'tty' else where})")
    if bash_only and not shell_is_bash:
        c.fail(f"{script}: &>> needs bash, but cron runs commands with /bin/sh unless the crontab sets SHELL=/bin/bash")
c.done()
