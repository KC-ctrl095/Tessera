import csv
import io
import json
from decimal import Decimal

from checklib import Check, WORK, original_text

c = Check()

# The truth, from the file as handed out.
totals = {}
for row in csv.DictReader(io.StringIO(original_text("sales.csv"))):
    if "2026-07-01" <= row["date"] <= "2026-09-30":
        rev = int(row["units"]) * Decimal(row["unit_price"].replace(",", ""))
        totals[row["region"]] = totals.get(row["region"], Decimal(0)) + rev
best = max(totals, key=totals.get)
want = totals[best]

p = WORK / "answer.json"
data = None
if not p.is_file():
    c.fail("answer.json is missing")
else:
    try:
        data = json.loads(p.read_text(encoding="utf-8"))
    except ValueError as e:
        c.fail(f"answer.json is not valid JSON: {e}")
if data is not None:
    if not isinstance(data, dict):
        c.fail(f"answer.json should hold an object, holds {type(data).__name__}")
    else:
        region = data.get("region")
        revenue = data.get("revenue")
        c.ok(
            isinstance(region, str) and region.strip().lower() == best.lower(),
            f"region is {best}" if isinstance(region, str) and region.strip().lower() == best.lower() else f"region should be {best}, is {region!r}",
        )
        if isinstance(revenue, bool) or not isinstance(revenue, (int, float)):
            c.fail(f"revenue should be a number, is {revenue!r}")
        else:
            close = abs(Decimal(str(revenue)) - want) <= Decimal("0.01")
            c.ok(close, f"revenue is {want}" if close else f"revenue should be {want}, is {revenue}")
c.done()
