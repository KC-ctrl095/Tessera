from checklib import Check, run, tail, with_hidden

c = Check()
with with_hidden() as d:
    code, out = run("sh tests/run_tests.sh", cwd=d, timeout=60)
    c.ok(code == 0, "the original tests pass" if code == 0 else "the original tests fail:\n" + tail(out))
    code, out = run("sh tests/run_more_tests.sh", cwd=d, timeout=60)
    c.ok(code == 0, "the extra tests pass (dash, glob and space names, empty source)" if code == 0 else "the extra tests fail:\n" + tail(out))
c.done()
