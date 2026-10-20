import importlib.util
import itertools
import math

from checklib import FILES, WORK, Check, original_text, run, tail, text, with_hidden

c = Check()


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


try:
    new = load(WORK / "rating.py", "rating_new")
except Exception as e:  # noqa: BLE001
    c.fail(f"rating.py doesn't import: {type(e).__name__}: {e}")
    c.done()
old = load(FILES / "rating.py", "rating_old")

# The original module, patched to the change request where a setting suffices.
old._bill_minutes = lambda seconds: 0 if seconds <= 0 else int(math.ceil(seconds / 60.0))
old.FAIR_USE_MB["ROW2"] = 1000


def expected(country, svc, qty, ptype, pname):
    if ptype == "business" and pname == "Premium":
        pname = "Gold"
    if country == "is":
        country = "no"
    if (country, svc) == ("no", "data"):
        billable = old._bill_mb(qty)
        price = billable * 0.10 + old._fair_use("EEA", billable)
        return old._apply_plan(price, ptype, pname)
    return old.RULES[(country, svc)](qty, ptype, pname)


c.ok(getattr(new, "TARIFF_VERSION", None) == "2026.4", "6. TARIFF_VERSION is 2026.4")
c.ok(new.FAIR_USE_MB == {"EU": 20000, "EEA": 20000, "ROW1": 500, "ROW2": 1000}, "5. ROW2 fair use is 1000 MB, the rest unchanged")
c.ok(new.FAIR_USE_SURCHARGE == old.FAIR_USE_SURCHARGE, "fair-use surcharges unchanged")

c.ok(new.PERSONAL_PLANS == old.PERSONAL_PLANS and new.PERSONAL_UPGRADE_PATH == ["Basic", "Silver", "Gold"], "2. personal plans unchanged")
want_business = {
    "Starter": {"multiplier": 0.95, "label": "Starter", "included_minutes": 0},
    "Premium": {"multiplier": 0.70, "label": "Premium", "included_minutes": 120},
    "Enterprise": {"multiplier": 0.55, "label": "Enterprise", "included_minutes": 600},
}
c.ok(new.BUSINESS_PLANS == want_business and list(new.BUSINESS_PLANS) == list(want_business), "2. business Gold is now Premium")
c.ok(new.BUSINESS_UPGRADE_PATH == ["Starter", "Premium", "Enterprise"], "2. the business upgrade path follows the rename")
try:
    ok_labels = new.plan_label("business", "Premium") == "Business Premium" and new.next_plan("business", "Starter") == "Premium"
except Exception:  # noqa: BLE001
    ok_labels = False
c.ok(ok_labels, "2. plan_label and next_plan know Premium")

want_countries = dict(old.COUNTRIES, **{"is": "EEA"})
c.ok(new.COUNTRIES == want_countries, "3. Iceland is an EEA country, the rest unchanged")
c.ok(list(new.COUNTRIES) == sorted(new.COUNTRIES), "3. COUNTRIES stays in alphabetical order")
services = ("voice", "voice_in", "sms", "mms", "data", "video")
c.ok(set(new.RULES) == set(old.RULES) | {("is", s) for s in services}, "3. RULES has Iceland's six services and nothing else new")

minutes_ok = all(new._bill_minutes(s) == m for s, m in [(0, 0), (-5, 0), (1, 1), (59.9, 1), (60, 1), (61, 2), (89, 2), (91, 2), (120, 2), (120.01, 3)])
c.ok(minutes_ok, "4. _bill_minutes counts every started minute")
helpers_ok = all(
    new._bill_seconds(x) == old._bill_seconds(x) and new._bill_mb(x) == old._bill_mb(x) for x in (0, 0.01, 0.5, 1, 1.04, 59.2, 3600.5)
) and all(new._bill_sms(n) == old._bill_sms(n) for n in (-3, 0, 1, 250))
c.ok(helpers_ok, "4. the other billing helpers are unchanged")

c.ok(not hasattr(new, "legacy_flat_rate"), "7. legacy_flat_rate is gone")
try:
    new.rate_event("pt", "voice", 60, "personal", "legacy")
    legacy_ok = False
except ValueError:
    legacy_ok = True
except Exception:  # noqa: BLE001
    legacy_ok = False
c.ok(legacy_ok, "7. a 'legacy' plan is an unknown plan (ValueError)")

quantities = {"voice": [0, 1, 61, 300, 3599], "voice_in": [0, 59, 61, 600], "sms": [0, 1, 99, 150], "mms": [0, 3],
              "data": [0, 0.05, 12.34, 480, 520, 990, 1100, 25000], "video": [0, 1, 61.5, 3700]}
plans = [("personal", "Basic"), ("personal", "Gold"), ("business", "Premium"), ("business", "Enterprise")]
wrong = []
for (country, svc) in sorted(new.RULES):
    if country not in want_countries:
        continue
    for qty, (ptype, pname) in itertools.product(quantities[svc], plans):
        try:
            got = new.rate_event(country, svc, qty, ptype, pname)
        except Exception as e:  # noqa: BLE001
            got = f"{type(e).__name__}"
        want = expected(country, svc, qty, ptype, pname)
        if not isinstance(got, (int, float)) or abs(got - want) > 1e-9:
            wrong.append(f"{country} {svc} {qty} {ptype}/{pname}: {got} instead of {want}")
c.ok(not wrong, "every price for every country, service and plan is as the change request says" if not wrong else f"{len(wrong)} prices are wrong, e.g. " + "; ".join(wrong[:5]))
try:
    gold = new.rate_event("pt", "voice", 60, "business", "Gold")
    c.fail(f"the business Gold plan still rates calls ({gold})")
except ValueError:
    c.note("business Gold is no longer a plan")
except Exception as e:  # noqa: BLE001
    c.fail(f"rating a business Gold call fails oddly: {type(e).__name__}")

n_old = len(original_text("rating.py").splitlines())
n_new = len((text("rating.py") or "").splitlines())
c.ok(n_old - 60 <= n_new <= n_old + 120, f"rating.py is whole ({n_new} lines, was {n_old})")
with with_hidden() as d:
    code, out = run("python3 -m unittest -q", cwd=d, timeout=60)
c.ok(code == 0, "the project's tests pass" if code == 0 else "tests fail:\n" + tail(out))
c.done()
