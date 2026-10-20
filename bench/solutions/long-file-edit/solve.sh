set -e
python3 - <<'PY'
p = "shipping_rules.py"
lines = open(p).read().split("\n")
start = next(i for i, l in enumerate(lines) if l.startswith("def zone_31_express"))
for i in range(start, start + 11):
    lines[i] = lines[i].replace("order_total >= 120", "order_total >= 150").replace("(weight_kg - 10) * 4.50", "(weight_kg - 10) * 4.75")
open(p, "w").write("\n".join(lines))
PY
