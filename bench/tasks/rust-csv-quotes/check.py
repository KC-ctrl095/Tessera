from checklib import Check, run, tail, with_hidden

c = Check()
with with_hidden() as d:
    code, out = run("cargo test --offline --quiet 2>&1", cwd=d, timeout=380, env={"CARGO_TERM_COLOR": "never", "CARGO_TARGET_DIR": str(d / "target")})
c.ok(code == 0, "the crate's tests and the full format tests pass" if code == 0 else "cargo test fails:\n" + tail(out, 3500))
c.done()
