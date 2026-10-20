import json

from checklib import HIDDEN, WORK, Check, original_text, run, tail, with_hidden

c = Check()
c.ok((WORK / "docs" / "fares-2026.csv").read_text(encoding="utf-8") == original_text("docs/fares-2026.csv"), "the fare table is unchanged")
with with_hidden(overlay=HIDDEN / "none") as d:
    code, out = run("python3 -m unittest -q", cwd=d, timeout=60)
    c.ok(code == 0, "the worked examples pass" if code == 0 else "the worked examples fail:\n" + tail(out, 1500))
    code, out = run(["python3", str(HIDDEN / "check_cases.py"), str(HIDDEN / "cases.json")], cwd=d, timeout=60)
if code != 0:
    c.fail("quote() crashed on the hidden bookings:\n" + tail(out))
else:
    r = json.loads(out.strip().splitlines()[-1])
    c.ok(r["wrong"] == 0, f"all {r['cases']} bookings priced as the spec says" if r["wrong"] == 0 else
         f"{r['wrong']} of {r['cases']} bookings are wrong, e.g. " + json.dumps(r["examples"][:2], ensure_ascii=False)[:1500])
c.done()
