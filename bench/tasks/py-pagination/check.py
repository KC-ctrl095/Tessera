from checklib import Check, run, tail, with_hidden

c = Check()
with with_hidden() as d:
    code, out = run("python3 -m unittest -v test_pagination test_pagination_more", cwd=d, timeout=60)
c.ok(code == 0, "the original and the extra paging tests pass" if code == 0 else "tests fail:\n" + tail(out))
c.done()
