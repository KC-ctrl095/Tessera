import importlib.util
import itertools

from checklib import FILES, WORK, Check, changed_regions, original_text

c = Check()


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


try:
    new = load(WORK / "shipping_rules.py", "rules_new")
except Exception as e:  # a broken file is a failure, not a crash
    c.fail(f"shipping_rules.py doesn't import: {type(e).__name__}: {e}")
    c.done()
old = load(FILES / "shipping_rules.py", "rules_old")


def expected(w, total):
    if total >= 150:
        return 0.0
    base = 9.80
    if w <= 2:
        return round(base, 2)
    if w <= 10:
        return round(base + (w - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (w - 10) * 4.75, 2)


weights = [0.5, 2, 2.5, 7, 10, 10.5, 12, 25.3]
totals = [0, 59.99, 119.99, 120, 130, 149.99, 150, 400]
wrong_target, others = [], []
for (zone, service), fn in old.RULES.items():
    new_fn = new.RULES.get((zone, service))
    if new_fn is None:
        others.append(f"zone {zone} {service} is missing")
        continue
    for w, t in itertools.product(weights, totals):
        got = new_fn(w, t)
        want = expected(w, t) if (zone, service) == (31, "express") else fn(w, t)
        if abs(got - want) > 1e-9:
            (wrong_target if (zone, service) == (31, "express") else others).append(f"zone {zone} {service} ({w} kg, {t} EUR): {got} instead of {want}")
c.ok(not wrong_target, "zone 31 express charges 4.75/kg above 10 kg and ships free from 150" if not wrong_target else "zone 31 express is wrong: " + "; ".join(wrong_target[:4]))
c.ok(not others, "every other rule is unchanged" if not others else f"{len(others)} other prices changed: " + "; ".join(others[:4]))
c.ok(len(new.RULES) == len(old.RULES), "the RULES table is complete")
c.ok(new.price(31, "express", 12, 50) == expected(12, 50), "price() uses the new rule")

# The edit must stay inside the zone 31 express function (the file is otherwise byte-for-byte the same).
lines = original_text("shipping_rules.py").splitlines()
start = next(i for i, l in enumerate(lines, 1) if l.startswith("def zone_31_express"))
end = next(i for i, l in enumerate(lines, 1) if i > start and l.startswith("def ")) - 1
outside = [(a, b) for a, b in changed_regions("shipping_rules.py") if not (start - 2 <= a and b <= end)]
c.ok(not outside, f"only lines {start}-{end} (the zone 31 express rule) changed" if not outside else f"lines outside the zone 31 express rule changed: {outside[:5]}")
c.done()
