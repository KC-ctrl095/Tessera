from checklib import Check, WORK, run, tail, with_hidden

c = Check()
c.ok((WORK / "inventory.ts").is_file(), "inventory.ts is still there")
c.ok(not (WORK / "inventory.js").exists(), "no JavaScript copy replaces it" if not (WORK / "inventory.js").exists() else "inventory.js was added; the module should stay in TypeScript")
with with_hidden() as d:
    code, out = run("node --test inventory.test.ts inventory.more.test.ts", cwd=d, timeout=120)
c.ok(code == 0, "the original and the extra inventory tests pass" if code == 0 else "tests fail:\n" + tail(out, 3000))
c.done()
