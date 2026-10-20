import re

from checklib import Check, run, tail, text, with_hidden

c = Check()
mk = text("Makefile") or ""
c.ok("\n    $(PYTHON)" not in mk and "\n  $(PYTHON)" not in mk, "recipes are indented with tabs")
with with_hidden() as d:
    code, out = run("make -n check", cwd=d, timeout=30)
    c.ok(code == 0 and re.search(r"python3 tools/lint\.py pages", out), "`make -n check` runs tools/lint.py pages with $(PYTHON)" if code == 0 else "make -n check fails:\n" + tail(out))
    # Start from a clean tree: the agent may have built the site already.
    import shutil

    shutil.rmtree(d / "site", ignore_errors=True)
    code, out = run("make PYTHON=python3 all", cwd=d, timeout=60)
    c.ok(code == 0, "`make all` works" if code == 0 else "make all fails:\n" + tail(out))
    lint_at = out.find("lint:")
    render_at = out.find("wrote site/index.html")
    c.ok(0 <= lint_at < render_at, "the check runs before the build")
    c.ok((d / "site" / "index.html").exists(), "the site is built")
    # A lint failure must stop `make all`.
    (d / "pages" / "04-bad.md").write_text("no title here \n")
    (d / "site" / "index.html").unlink(missing_ok=True)
    code, out = run("make all", cwd=d, timeout=60)
    c.ok(code != 0 and not (d / "site" / "index.html").exists(), "a failing check stops `make all`")
    code, out = run("make -n PYTHON=/opt/py/bin/python check", cwd=d, timeout=30)
    c.ok("/opt/py/bin/python tools/lint.py pages" in out, "the check target uses $(PYTHON)")
phony = re.search(r"(?m)^\.PHONY:.*$", mk)
c.ok(bool(re.search(r"(?m)^\.PHONY:.*\bcheck\b", mk)), ".PHONY lists check")
c.done()
