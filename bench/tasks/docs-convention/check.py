import ast
import subprocess
import json
import os
import re
import tempfile
from pathlib import Path

from checklib import WORK, Check, original_text, run, tail, text, with_hidden

c = Check()

# The command module: docstring with a help line, and run(args).
src = text("notekit/commands/count.py")
if not c.ok(src is not None, "notekit/commands/count.py exists" if src is not None else "notekit/commands/count.py is missing"):
    c.done()
try:
    tree = ast.parse(src)
except SyntaxError as e:
    c.fail(f"count.py doesn't parse: {e}")
    c.done()
doc = ast.get_docstring(tree) or ""
first = doc.strip().splitlines()[0].strip() if doc.strip() else ""
c.ok(first != "", f"count.py's docstring gives the help line: {first!r}" if first else "count.py has no module docstring with a help line")
runs = [n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "run" and len(n.args.args) == 1]
c.ok(runs, "count.py has run(args)" if runs else "count.py has no run(args) function")

with with_hidden() as d:
    # Registered, in alphabetical order.
    code, out = run(
        "python3 -c 'import json; from notekit.commands import COMMANDS; "
        "print(json.dumps([[k, v.__name__] for k, v in COMMANDS.items()]))'",
        cwd=d,
        timeout=30,
    )
    try:
        reg = json.loads(out.strip().splitlines()[-1])
    except (ValueError, IndexError):
        reg = None
    names = [k for k, _ in reg] if reg else []
    c.ok(
        reg is not None and ["count", "notekit.commands.count"] in reg,
        "count is registered in COMMANDS" if reg else "COMMANDS couldn't be loaded:\n" + tail(out),
    )
    c.ok(names == sorted(names) and len(names) == 3, f"COMMANDS is in alphabetical order: {names}" if names == sorted(names) else f"COMMANDS isn't in alphabetical order: {names}")

    # The command itself, on notes it hasn't seen.
    with tempfile.TemporaryDirectory(prefix="bench-notes-") as tmp:
        notes = Path(tmp) / "notes"
        notes.mkdir()
        files = {
            "standups.md": "tags: work, ideas\n# Better standups\n",
            "bikes.md": "tags: Ideas, home\n# Cargo bike\n",
            "groceries.md": "# Groceries\n- oats\n",
            "garden.md": "  tags: home\n# Garden plan\n",
            "tax-letter.md": "tags: paperwork\n# Letter to the tax office\n",
            "todo.txt": "not a note\n",
        }
        for name, body in files.items():
            (notes / name).write_text(body, encoding="utf-8")
        empty = Path(tmp) / "empty"
        empty.mkdir()
        cases = [
            ([], 5, notes),
            (["--tag", "ideas"], 2, notes),
            (["--tag", "Home"], 2, notes),
            (["--tag", "paperwork"], 1, notes),
            (["--tag", "travel"], 0, notes),
            ([], 0, empty),
        ]
        for extra, want, folder in cases:
            argv = " ".join(["python3 -m notekit --dir", str(folder), "count", *extra])
            code, out = run(argv, cwd=d, timeout=30, env={"LC_ALL": "C.UTF-8"})
            nums = re.findall(r"\d+", out)
            good = code == 0 and nums == [str(want)]
            shown = " ".join(["count", *extra])
            c.ok(good, f"`{shown}` on the {folder.name} folder prints {want}" if good else f"`{argv}` should print {want} (exit 0), got exit {code}:\n{tail(out, 600)}")

        missing = Path(tmp) / "no-such-folder"
        p = subprocess.run(
            ["python3", "-m", "notekit", "--dir", str(missing), "count"],
            cwd=d,
            capture_output=True,
            text=True,
            timeout=30,
            env={**{k: v for k, v in os.environ.items() if k != "PYTHONPATH"}, "PYTHONDONTWRITEBYTECODE": "1"},
        )
        good = p.returncode == 2 and p.stderr.startswith("notekit: error: ") and p.stdout == "" and "Traceback" not in p.stderr
        c.ok(
            good,
            "a missing notes folder gives `notekit: error: ...` on stderr and exit code 2"
            if good
            else f"a missing notes folder should give `notekit: error: ...` on stderr and exit code 2; got exit {p.returncode}, stderr {p.stderr[:300]!r}, stdout {p.stdout[:200]!r}",
        )

        code, out = run("python3 -m notekit --help", cwd=d, timeout=30)
        c.ok(re.search(r"^\s+count\s+" + re.escape(first[:30]), out, re.M) is not None, "`--help` lists count with its help line" if first else "`--help` doesn't list count")

    # The tests that come with it.
    test_count = Path(d) / "tests" / "test_count.py"
    if c.ok(test_count.is_file(), "tests/test_count.py exists" if test_count.is_file() else "tests/test_count.py is missing"):
        code, out = run("python3 -m unittest -v tests.test_count", cwd=d, timeout=60)
        m = re.search(r"^Ran (\d+) tests?", out, re.M)
        ran = int(m.group(1)) if m else 0
        c.ok(code == 0 and ran >= 2, f"tests/test_count.py passes ({ran} tests)" if code == 0 and ran >= 2 else f"tests/test_count.py should pass with at least two tests (count and --tag); exit {code}, ran {ran}:\n{tail(out)}")

    # The existing commands still work (their original tests).
    code, out = run("python3 -m unittest -v tests.test_list tests.test_show", cwd=d, timeout=60)
    c.ok(code == 0, "list and show still pass their tests" if code == 0 else "list/show tests fail:\n" + tail(out))

# The changelog line, under Unreleased, and nothing else lost.
log = text("CHANGELOG.md") or ""
section, current = {}, None
for line in log.splitlines():
    if line.startswith("## "):
        current = line[3:].strip()
        section[current] = []
    elif current is not None:
        section[current].append(line.rstrip())
unreleased = section.get("Unreleased", [])
entry = [line for line in unreleased if re.fullmatch(r"- Added `count` command: \S.*\.", line)]
c.ok(len(entry) == 1, f"CHANGELOG has the entry under Unreleased: {entry[0]!r}" if len(entry) == 1 else "CHANGELOG needs one line `- Added `count` command: <what it does>.` under `## Unreleased`")
elsewhere = [k for k, v in section.items() if k != "Unreleased" and any("count" in line for line in v)]
c.ok(not elsewhere, "the entry isn't filed under a released version" if not elsewhere else f"count is mentioned under released versions: {elsewhere}")
orig = [line for line in original_text("CHANGELOG.md").splitlines() if line.strip()]
kept = [line for line in log.splitlines() if line.strip() in {o.strip() for o in orig}]
c.ok([k.strip() for k in kept] == [o.strip() for o in orig], "the earlier changelog lines are all still there" if [k.strip() for k in kept] == [o.strip() for o in orig] else "earlier CHANGELOG lines were changed or removed")
c.done()
