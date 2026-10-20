"""Worked examples (spec §11 + visible tests) and hidden cases, computed by the reference solution."""

import json
import random
import shutil
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
tmp = Path(tempfile.mkdtemp())
(tmp / "docs").mkdir()
shutil.copy(HERE / "files/docs/fares-2026.csv", tmp / "docs")
shutil.copy(HERE / "../../solutions/spec-fares/fares.py", tmp)
sys.path.insert(0, str(tmp))
import fares  # noqa: E402

EXAMPLES = [
    ("A family in summer, at peak hour, with a code", {"route": "CS-SE", "date": "2026-07-14", "time": "08:15",
     "passengers": [{"type": "adult"}, {"type": "adult"}, {"type": "child", "age": 7}, {"type": "child", "age": 9}, {"type": "infant", "age": 1}], "promo": "SUMMER10"},
     "Tuesday 08:15 is peak hour: each adult pays 2.95 × 115%. Both children travel free (§5), the infant too (§3.2). Subtotal 6.785 → 6.79; SUMMER10 takes 0.68; plus the 0.50 booking fee."),
    ("A night crossing by car, return in ten days, with a code that doesn't combine", {"route": "LI-PO", "class": "comfort", "date": "2026-05-12", "time": "23:30",
     "return": {"date": "2026-05-22", "time": "01:15"}, "passengers": [{"type": "adult"}, {"type": "adult"}], "vehicles": [{"type": "car"}], "promo": "NIGHTOWL"},
     "Outbound 2 × 64.00 + 18.00 = 146.00. The return departs at 01:15 (night): 2 × 64.00 × 80% + 18.00 = 120.40, return discount 12.04. NIGHTOWL (25% of 266.40 = 66.60) doesn't combine and is larger, so it replaces the return discount."),
    ("A group, paid in cash at the counter", {"route": "SE-TR", "date": "2026-03-07", "time": "10:00",
     "passengers": [{"type": "adult"}] * 9 + [{"type": "senior", "age": 71}, {"type": "student", "card": True}, {"type": "infant", "age": 2}],
     "channel": "counter", "payment": "cash"},
     "A Saturday: the student pays the adult fare. Eleven passengers pay (the infant is free), so the group gets 12% off; no booking fee at the counter; cash rounds to 0.05."),
    ("Weekday students and seniors at peak hour", {"route": "TP-BA", "date": "2026-10-06", "time": "18:05",
     "passengers": [{"type": "student", "card": True}, {"type": "senior", "age": 66}, {"type": "senior", "age": 60}]},
     "Student 3.40 × 75% × 115%, senior 3.40 × 70% × 115%, and the 60-year-old 'senior' pays as an adult: 3.40 × 115%."),
    ("A van and a bicycle", {"route": "BE-TR", "date": "2026-08-20", "time": "12:00",
     "passengers": [{"type": "adult"}], "vehicles": [{"type": "van", "length_m": 6.4}, {"type": "bicycle"}]},
     "High season: 1.70 + (22.00 + 2 × 3.00) + 2.00."),
    ("The discount cap", {"route": "CS-MO", "date": "2026-11-03", "time": "11:00", "return": {"date": "2026-11-04", "time": "11:00"},
     "passengers": [{"type": "adult"}] * 10, "promo": "PARTNER45"},
     "Each leg 10 × 3.20 = 32.00. Group: 3.84 off each leg (7.68); return: 10% of 28.16 = 2.82; PARTNER45 combines: 45% of 53.50 = 24.08. Together 34.58, more than 40% of 64.00, so the discounts are capped at 25.60: 64.00 − 25.60 + 0.50."),
]


def quote_or_error(b):
    try:
        return fares.quote(b)
    except ValueError as e:
        return {"error": "ValueError"}


examples_md = []
for title, booking, why in EXAMPLES:
    q = fares.quote(booking)
    examples_md.append(f"### {title}\n\n```python\nquote({json.dumps(booking, ensure_ascii=False)})\n# -> {json.dumps(q)}\n```\n\n{why}\n")
spec_path = HERE / "files/docs/FARES-SPEC.md"
spec = spec_path.read_text(encoding="utf-8")
marker = "## 11. Worked examples\n\n"
start = spec.index(marker) + len(marker)
end = spec.index("## 12. History")
spec_path.write_text(spec[:start] + "\n".join(examples_md) + "\n" + spec[end:], encoding="utf-8")
visible = [(title, booking, fares.quote(booking)) for title, booking, _ in EXAMPLES]
(HERE / "files/tests/examples.json").write_text(json.dumps([{"title": t, "booking": b, "quote": q} for t, b, q in visible], indent=1, ensure_ascii=False))

rng = random.Random(77)
routes = list(fares.ROUTES)
types = ["adult"] * 6 + ["child", "child", "infant", "senior", "student", "disabled", "companion"]


def rand_passenger():
    t = rng.choice(types)
    p = {"type": t}
    if t in ("child", "infant", "adult") and rng.random() < 0.6:
        p["age"] = {"child": rng.randint(2, 13), "infant": rng.randint(0, 5), "adult": rng.randint(10, 80)}[t]
    if t == "senior":
        p["age"] = rng.choice([60, 64, 65, 70, 82])
    if t == "student":
        p["card"] = rng.random() < 0.8
    return p


def rand_date():
    from datetime import date, timedelta
    special = ["2026-06-14", "2026-06-15", "2026-09-15", "2026-09-16", "2026-12-23", "2026-12-24", "2026-04-25", "2026-01-01", "2026-06-13", "2026-10-05"]
    if rng.random() < 0.3:
        return rng.choice(special)
    return (date(2026, 1, 1) + timedelta(days=rng.randrange(365))).isoformat()


def rand_time():
    special = ["06:59", "07:00", "09:29", "09:30", "16:59", "17:00", "19:29", "19:30", "00:00", "04:59", "05:00", "23:59"]
    return rng.choice(special) if rng.random() < 0.4 else f"{rng.randrange(24):02d}:{rng.randrange(60):02d}"


cases = []
for i in range(220):
    code = rng.choice(routes)
    name, carries, classes = fares.ROUTES[code]
    cls = "comfort" if classes["comfort"] and rng.random() < 0.35 else "standard"
    n = rng.choice([1, 1, 2, 3, 4, 5, 6, 9, 10, 11, 12])
    b = {"route": code, "class": cls, "date": rand_date(), "time": rand_time(), "passengers": [rand_passenger() for _ in range(n)]}
    if rng.random() < 0.3:  # families
        b["passengers"] = [{"type": "adult"}, {"type": "adult", "age": 40}] + [{"type": "child", "age": rng.randint(4, 11)} for _ in range(rng.randint(2, 5))] + b["passengers"][:2]
    if carries and rng.random() < 0.5:
        b["vehicles"] = [rng.choice([{"type": "car"}, {"type": "motorcycle"}, {"type": "bicycle"}, {"type": "van", "length_m": rng.choice([4.8, 5.0, 5.01, 6.4, 7.0, 8.95])}]) for _ in range(rng.randint(1, 2))]
    if rng.random() < 0.5:
        from datetime import date, timedelta
        d = date.fromisoformat(b["date"]) + timedelta(days=rng.choice([0, 1, 5, 29, 30, 31, 45]))
        b["return"] = {"date": d.isoformat(), "time": rand_time()}
    if rng.random() < 0.6:
        b["promo"] = rng.choice(list(fares.PROMOS) + ["summer10", "NOPE"])
    if rng.random() < 0.3:
        b["channel"] = "counter"
    if rng.random() < 0.3:
        b["payment"] = "cash"
    cases.append({"booking": b, "quote": quote_or_error(b)})
for b in [
    {"route": "XX-YY", "date": "2026-05-05", "time": "10:00", "passengers": [{"type": "adult"}]},
    {"route": "CS-CA", "class": "comfort", "date": "2026-05-05", "time": "10:00", "passengers": [{"type": "adult"}]},
    {"route": "CS-BA", "date": "2026-05-05", "time": "10:00", "passengers": [{"type": "adult"}], "vehicles": [{"type": "car"}]},
    {"route": "BE-TR", "date": "2026-05-05", "time": "10:00", "passengers": [{"type": "adult"}], "vehicles": [{"type": "tractor"}]},
    {"route": "CS-BA", "date": "2026-05-05", "time": "10:00", "passengers": []},
    {"route": "CS-BA", "date": "2026-05-05", "time": "10:00", "return": {"date": "2026-05-04", "time": "10:00"}, "passengers": [{"type": "adult"}]},
]:
    cases.append({"booking": b, "quote": quote_or_error(b)})
(HERE / "hidden/cases.json").write_text(json.dumps(cases, indent=0, ensure_ascii=False))
kinds = {"errors": sum("error" in c["quote"] for c in cases), "promo applied": sum(c["quote"].get("promo_applied", False) for c in cases),
         "cash": sum(c["booking"].get("payment") == "cash" for c in cases)}
print(len(cases), "cases", kinds)
shutil.rmtree(tmp)
