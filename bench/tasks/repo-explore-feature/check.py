from checklib import WORK, Check, run, tail, with_hidden

c = Check()
tests = " ".join(p.read_text(errors="replace") for p in (WORK / "tests").glob("test_*.py"))
c.ok("everse" in tests or "reverse_charge" in tests, "the project's tests cover the reverse charge")
with with_hidden() as d:
    code, out = run("python3 -m unittest discover -s tests -t . -v", cwd=d, timeout=90)
c.ok(code == 0, "the project's tests and the reverse-charge tests pass" if code == 0 else "tests fail:\n" + tail(out, 3500))
c.done()
