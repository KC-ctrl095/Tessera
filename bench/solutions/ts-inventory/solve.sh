set -e
python3 - <<'PY'
p = "inventory.ts"
s = open(p).read()
s = s.replace("  const next = stock;\n", "  const next: Stock = { ...stock };\n")
s = s.replace("    if (available > line.qty) {\n", "    if (available >= line.qty) {\n")
s = s.replace("return { ok: false, stock: next, error: `not enough", "return { ok: false, stock, error: `not enough")
open(p, "w").write(s)
PY
