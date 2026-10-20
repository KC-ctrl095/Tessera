import copy
import json

from checklib import Check, original_text, text

REL = "config/services.json"
BILLING = {
    "name": "billing",
    "url": "https://billing.internal:8443",
    "timeout_ms": 5000,
    "retries": 3,
    "tags": ["payments"],
}

c = Check()
raw = text(REL)
try:
    data = json.loads(raw) if raw is not None else None
except ValueError as e:
    data = None
    c.fail(f"{REL} is not valid JSON: {e}")
if raw is None:
    c.fail(f"{REL} is missing")

if data is not None:
    orig = json.loads(original_text(REL))
    expected = copy.deepcopy(orig)
    for s in expected["services"]:
        if s["name"] == "search":
            s["timeout_ms"] = 15000

    services = data.get("services") if isinstance(data, dict) else None
    if not isinstance(services, list):
        c.fail("there is no services list any more")
    else:
        billing = [s for s in services if isinstance(s, dict) and s.get("name") == "billing"]
        c.ok(len(billing) == 1, f"one billing service (found {len(billing)})")
        if billing:
            c.ok(billing[0] == BILLING, f"billing has the requested settings (got {json.dumps(billing[0])})")
        rest = [s for s in services if not (isinstance(s, dict) and s.get("name") == "billing")]
        if rest == expected["services"]:
            c.note("the other services are unchanged and in their order; search has timeout_ms 15000")
        else:
            for want, got in zip(expected["services"], rest):
                if want != got:
                    c.fail(f"service {want['name']} should be {json.dumps(want)}, is {json.dumps(got)}")
                    break
            else:
                c.fail(f"expected {len(expected['services'])} other services, found {len(rest)}")
        others = {k: v for k, v in data.items() if k != "services"}
        want_others = {k: v for k, v in expected.items() if k != "services"}
        c.ok(others == want_others, "everything outside the services list is unchanged")

    # The file stays hand-readable: indented with two spaces, no tabs.
    lines = raw.splitlines()
    indents = [len(line) - len(line.lstrip(" ")) for line in lines if line.strip()]
    c.ok("\t" not in raw, "no tabs")
    c.ok(all(i % 2 == 0 for i in indents), "every indent is a multiple of two spaces")
    nonzero = [i for i in indents if i]
    c.ok(len(lines) >= 40 and nonzero and min(nonzero) == 2, "still pretty-printed with a two-space indent")
c.done()
