from checklib import Check, run, tail, with_hidden

c = Check()
with with_hidden() as d:
    code, out = run(["node", "--test", "money.test.js", "money.more.test.js"], cwd=d, timeout=60)
c.ok(code == 0, "the original and the extra price tests pass" if code == 0 else "tests fail:\n" + tail(out, 3000))
c.done()
