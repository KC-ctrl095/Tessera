"""Run in the scratch copy: compare the agent's find_duplicates with the original, then time it."""

import importlib.util
import inspect
import json
import random
import sys
import time

sys.path.insert(0, ".")
import dedupe  # noqa: E402

spec = importlib.util.spec_from_file_location("dedupe_original", sys.argv[1])
original = importlib.util.module_from_spec(spec)
spec.loader.exec_module(original)

report = {"signature": str(inspect.signature(dedupe.find_duplicates)), "mismatches": [], "seconds": None}

EMAILS = ["ana@x.org", "Ana@X.org ", "a.na@gmail.com", "ana+1@gmail.com", "ANA@googlemail.com", "bo@y.net", "bo+@y.net", "+x@y.net", "", "  ", "two@@at.org", "c@z.io"]
NAMES = ["Lee Chan", "lee  chan", "LEE\tCHAN", "Ｌｅｅ Chan", "Mia", "mia ", "", "Zoë", "ZOË", "  "]
CITIES = ["Porto", "porto", " Porto", "Braga", "", 7, "7", None]


def rand_record(rng, i):
    r = {"id": i}
    for field, pool in (("email", EMAILS), ("name", NAMES), ("city", CITIES)):
        roll = rng.random()
        if roll < 0.1:
            continue  # missing
        if roll < 0.15:
            r[field] = None
        else:
            r[field] = rng.choice(pool)
    return r


rng = random.Random(1234)
for trial in range(400):
    n = rng.randint(0, 30)
    recs = [rand_record(rng, i) for i in range(n)]
    fields = rng.choice([("email",), ("name",), ("city",), ("name", "city"), ("city", "email"), ("email", "name", "city")])
    norm = rng.random() < 0.7
    want = original.find_duplicates(recs, key_fields=fields, normalize=norm)
    try:
        got = dedupe.find_duplicates(recs, key_fields=fields, normalize=norm)
        same = isinstance(got, list) and len(got) == len(want) and all(
            isinstance(g, list) and len(g) == len(w) and all(x is y for x, y in zip(g, w)) for g, w in zip(got, want)
        )
    except Exception as e:  # noqa: BLE001
        got, same = f"{type(e).__name__}: {e}", False
    if not same:
        ids = lambda gs: [[r["id"] for r in g] for g in gs] if isinstance(gs, list) else gs  # noqa: E731
        report["mismatches"].append({"fields": fields, "normalize": norm, "want": ids(want), "got": ids(got) if isinstance(got, list) else got})
        if len(report["mismatches"]) >= 5:
            break

# Default arguments must behave the same too.
recs = [{"email": "a@b.c"}, {"email": "A@B.C"}, {"name": "x"}]
if [[id(r) for r in g] for g in dedupe.find_duplicates(recs)] != [[id(r) for r in g] for g in original.find_duplicates(recs)]:
    report["mismatches"].append({"defaults": "differ"})

big_rng = random.Random(99)
big = []
for i in range(50_000):
    if i > 0 and big_rng.random() < 0.06:
        src = big[big_rng.randrange(len(big))]
        big.append({"id": i, "email": src["email"].upper() + " ", "name": src["name"]})
    else:
        big.append({"id": i, "email": f"user{i}.{big_rng.randrange(10**6)}@example{i % 50}.org", "name": f"Person {i}"})
t0 = time.time()
groups = dedupe.find_duplicates(big)
report["seconds"] = round(time.time() - t0, 2)
report["big_groups"] = len(groups)
print(json.dumps(report))
