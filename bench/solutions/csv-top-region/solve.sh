set -e
python3 - <<'PY'
import csv, json
from decimal import Decimal
tot = {}
with open("sales.csv", newline="") as f:
    for r in csv.DictReader(f):
        if "2026-07-01" <= r["date"] <= "2026-09-30":
            tot[r["region"]] = tot.get(r["region"], Decimal(0)) + int(r["units"]) * Decimal(r["unit_price"].replace(",", ""))
best = max(tot, key=tot.get)
json.dump({"region": best, "revenue": float(tot[best])}, open("answer.json", "w"))
PY
