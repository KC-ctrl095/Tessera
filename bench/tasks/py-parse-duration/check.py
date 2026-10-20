from checklib import Check, run, tail, with_hidden

c = Check()
with with_hidden() as d:
    code, out = run("python3 -m unittest -v test_durations test_durations_spec", cwd=d, timeout=60)
c.ok(code == 0, "every rule in the spec holds" if code == 0 else "spec tests fail:\n" + tail(out, 3000))
c.done()
