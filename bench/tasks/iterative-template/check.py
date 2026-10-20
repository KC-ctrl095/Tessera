from checklib import Check, run, tail, tree_digest, original_tree_digest, WORK, with_hidden

c = Check()
c.ok(tree_digest(WORK / "cases") == original_tree_digest("cases"), "cases/ is unchanged")
with with_hidden() as d:
    code, out = run("python3 run_tests.py cases", cwd=d, timeout=60)
c.ok(code == 0, "all template cases pass, including more nesting, escaping and loop cases" if code == 0 else "a case fails:\n" + tail(out, 1500))
c.done()
