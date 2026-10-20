set -e
python3 - <<'PY'
p = "money.js"
s = open(p, encoding="utf-8").read()
old = """  const amount = cents / 100;
  // Whole amounts get their two decimals; the rest are already precise.
  const text = Number.isInteger(amount) ? amount.toFixed(2) : String(amount);
  return symbol + text;"""
new = """  // Work in whole cents so floating point never rounds a price.
  const sign = cents < 0 ? '-' : '';
  const abs = Math.abs(Math.round(cents));
  const whole = String(Math.floor(abs / 100)).replace(/\\B(?=(\\d{3})+(?!\\d))/g, ',');
  const fraction = String(abs % 100).padStart(2, '0');
  return `${sign}${symbol}${whole}.${fraction}`;"""
assert old in s
open(p, "w", encoding="utf-8").write(s.replace(old, new))
PY
