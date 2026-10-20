import re

from checklib import FILES, WORK, Check, copy_files, original_tree_digest, run, tail, tree_digest, unchanged, with_hidden

c = Check()
c.ok(tree_digest(WORK / "tests") == original_tree_digest("tests"), "tests/ is unchanged" if tree_digest(WORK / "tests") == original_tree_digest("tests") else "tests/ was changed (the task was to fix the code, not the tests)")
c.ok(unchanged("run_tests.py"), "run_tests.py is unchanged" if unchanged("run_tests.py") else "run_tests.py was changed")

# The project's own tests and runner as handed out, plus extra checks, on the agent's code.
with with_hidden() as d:
    copy_files(FILES / "tests", d / "tests")
    copy_files(FILES / "run_tests.py", d / "run_tests.py")
    code, out = run(["python3", "run_tests.py"], cwd=d, timeout=150)

bad = re.findall(r"^\[\s*\d+/\d+\] (\S+) \.\.\. (FAIL|ERROR) ", out, re.M)
ran = re.search(r"^Ran (\d+) tests", out, re.M)
blocks = re.findall(r"^(?:FAIL|ERROR): \S+\n-{70}\n(.*?)^={70}$", out, re.M | re.S)
total = int(ran.group(1)) if ran else 0
if code == 0 and not bad and total >= 133:
    c.note(f"all {total} tests pass (the project's 121 and extra checks)")
else:
    detail = "\n".join(f"  {name} {kind}" for name, kind in bad) or tail(out, 1500)
    if blocks:
        detail += "\nfirst failure:\n" + tail(blocks[0], 1200)
    c.fail(f"tests fail ({len(bad)} failing, {total} ran, exit {code}):\n{detail}")
c.done()
