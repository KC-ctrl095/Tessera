import json

from checklib import FILES, HIDDEN, WORK, Check, original_tree_digest, run, tail, tree_digest, with_hidden

c = Check()
c.ok(tree_digest(WORK / "tests") == original_tree_digest("tests"), "tests/ is unchanged")
c.ok(tree_digest(WORK / "vendor", skip={"__pycache__"}) == original_tree_digest("vendor"), "vendor/ is unchanged")
with with_hidden(overlay=HIDDEN / "none") as d:
    code, out = run("python3 -m unittest discover -s tests -t .", cwd=d, timeout=60)
    c.ok(code == 0, "the project's tests pass" if code == 0 else "the project's tests fail:\n" + tail(out))
    code, out = run(["python3", str(HIDDEN / "check_dedupe.py"), str(FILES / "dedupe.py")], cwd=d, timeout=40)
if code != 0:
    c.fail("find_duplicates didn't finish in time or crashed:\n" + tail(out))
    c.done()
r = json.loads(out.strip().splitlines()[-1])
c.ok(r["signature"] == "(records, key_fields=('email',), normalize=True)", f"the signature is unchanged ({r['signature']})")
c.ok(not r["mismatches"], "same groups, order and objects as before on 400 random inputs" if not r["mismatches"] else f"output differs from the original: {r['mismatches'][:3]}")
c.ok(r["seconds"] is not None and r["seconds"] < 10, f"50,000 records take {r['seconds']} s (limit 10 s)")
c.done()
