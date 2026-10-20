"""Generates files/rating.py, a big hand-maintained-looking rating engine (committed output)."""

import random

rng = random.Random(1404)
SERVICES = ("voice", "voice_in", "sms", "mms", "data", "video")
COUNTRIES = [
    ("at", "Austria", "EU"), ("ba", "Bosnia and Herzegovina", "ROW1"), ("be", "Belgium", "EU"), ("bg", "Bulgaria", "EU"),
    ("br", "Brazil", "ROW2"), ("ca", "Canada", "ROW2"), ("ch", "Switzerland", "ROW1"), ("cl", "Chile", "ROW2"),
    ("cy", "Cyprus", "EU"), ("cz", "Czechia", "EU"), ("de", "Germany", "EU"), ("dk", "Denmark", "EU"),
    ("ee", "Estonia", "EU"), ("eg", "Egypt", "ROW2"), ("es", "Spain", "EU"), ("fi", "Finland", "EU"),
    ("fr", "France", "EU"), ("gb", "United Kingdom", "ROW1"), ("gr", "Greece", "EU"), ("hr", "Croatia", "EU"),
    ("hu", "Hungary", "EU"), ("ie", "Ireland", "EU"), ("il", "Israel", "ROW2"), ("in", "India", "ROW2"),
    ("it", "Italy", "EU"), ("jp", "Japan", "ROW2"), ("ke", "Kenya", "ROW2"), ("kr", "South Korea", "ROW2"),
    ("li", "Liechtenstein", "EEA"), ("lt", "Lithuania", "EU"), ("lu", "Luxembourg", "EU"), ("lv", "Latvia", "EU"),
    ("ma", "Morocco", "ROW1"), ("md", "Moldova", "ROW1"), ("me", "Montenegro", "ROW1"), ("mk", "North Macedonia", "ROW1"),
    ("mt", "Malta", "EU"), ("mx", "Mexico", "ROW2"), ("nl", "Netherlands", "EU"), ("no", "Norway", "EEA"),
    ("nz", "New Zealand", "ROW2"), ("pl", "Poland", "EU"), ("pt", "Portugal", "EU"), ("ro", "Romania", "EU"),
    ("rs", "Serbia", "ROW1"), ("se", "Sweden", "EU"), ("si", "Slovenia", "EU"), ("sk", "Slovakia", "EU"),
    ("th", "Thailand", "ROW2"), ("tn", "Tunisia", "ROW1"), ("tr", "Turkey", "ROW1"), ("ua", "Ukraine", "ROW1"),
    ("us", "United States", "ROW2"), ("za", "South Africa", "ROW2"), ("ad", "Andorra", "ROW1"), ("al", "Albania", "ROW1"),
    ("ar", "Argentina", "ROW2"), ("au", "Australia", "ROW2"), ("cn", "China", "ROW2"), ("co", "Colombia", "ROW2"),
    ("ge", "Georgia", "ROW1"), ("id", "Indonesia", "ROW2"), ("is_", "", ""),
]
COUNTRIES = sorted([c for c in COUNTRIES if c[1]], key=lambda c: c[0])
ZONE_RATES = {
    "EU": {"voice": [0.0], "sms": [0.0], "data": [0.0]},
    "EEA": {"voice": [0.12, 0.15], "sms": [0.05, 0.06], "data": [0.12]},
    "ROW1": {"voice": [0.49, 0.59, 0.69], "sms": [0.19, 0.25], "data": [1.49, 1.99]},
    "ROW2": {"voice": [1.29, 1.49, 1.99], "sms": [0.29, 0.39], "data": [4.99, 5.99, 6.99]},
}

out = []
w = out.append
w('''"""Roaming rating engine: what a customer pays for calls, texts and data abroad.

One function per country and service, so that the pricing team can review a
country's prices in one place. Prices are in euros. Plans change prices by a
multiplier (see PERSONAL_PLANS and BUSINESS_PLANS).

Maintained by the billing team. Change requests: docs/CR-*.md.
"""

import math

TARIFF_VERSION = "2026.3"

# Zone of every country we rate. EU: regulated "roam like at home".
# EEA: EU rules by agreement. ROW1/ROW2: rest of the world, near and far.
COUNTRIES = {
''')
for code, name, zone in COUNTRIES:
    w(f'    "{code}": "{zone}",  # {name}\n')
w('''}

# Monthly data volume (MB) after which the fair-use surcharge applies, per zone.
FAIR_USE_MB = {
    "EU": 20000,
    "EEA": 20000,
    "ROW1": 500,
    "ROW2": 500,
}
# Surcharge per MB above the fair-use volume.
FAIR_USE_SURCHARGE = {
    "EU": 0.0025,
    "EEA": 0.0025,
    "ROW1": 0.50,
    "ROW2": 0.50,
}

PERSONAL_PLANS = {
    "Basic": {"multiplier": 1.00, "label": "Basic", "included_minutes": 0},
    "Silver": {"multiplier": 0.90, "label": "Silver", "included_minutes": 30},
    "Gold": {"multiplier": 0.75, "label": "Gold", "included_minutes": 60},
}
BUSINESS_PLANS = {
    "Starter": {"multiplier": 0.95, "label": "Starter", "included_minutes": 0},
    "Gold": {"multiplier": 0.70, "label": "Gold", "included_minutes": 120},
    "Enterprise": {"multiplier": 0.55, "label": "Enterprise", "included_minutes": 600},
}
PERSONAL_UPGRADE_PATH = ["Basic", "Silver", "Gold"]
BUSINESS_UPGRADE_PATH = ["Starter", "Gold", "Enterprise"]


def plan_info(plan_type: str, plan_name: str) -> dict:
    """The plan's settings; ValueError for an unknown plan."""
    plans = {"personal": PERSONAL_PLANS, "business": BUSINESS_PLANS}.get(plan_type)
    if plans is None or plan_name not in plans:
        raise ValueError(f"unknown plan: {plan_type}/{plan_name}")
    return plans[plan_name]


def next_plan(plan_type: str, plan_name: str):
    """The plan to suggest as an upgrade, or None at the top."""
    path = PERSONAL_UPGRADE_PATH if plan_type == "personal" else BUSINESS_UPGRADE_PATH
    i = path.index(plan_name)
    return path[i + 1] if i + 1 < len(path) else None


def _bill_minutes(seconds: float) -> int:
    """Billable minutes: every started minute counts."""
    if seconds <= 0:
        return 0
    return int(round(seconds / 60.0))


def _bill_seconds(seconds: float) -> int:
    """Billable seconds for per-second tariffs: every started second counts."""
    if seconds <= 0:
        return 0
    return int(math.ceil(seconds))


def _bill_sms(count: int) -> int:
    """Billable messages (negative counts are corrections and bill nothing)."""
    return max(0, int(count))


def _bill_mb(mb: float) -> float:
    """Billable data: rounded up to the next 0.1 MB."""
    if mb <= 0:
        return 0.0
    return math.ceil(mb * 10) / 10


def _apply_plan(price: float, plan_type: str, plan_name: str) -> float:
    return round(price * plan_info(plan_type, plan_name)["multiplier"], 2)


def _fair_use(zone: str, billable_mb: float) -> float:
    """The fair-use surcharge for this much data in a month."""
    over = max(0.0, billable_mb - FAIR_USE_MB[zone])
    return over * FAIR_USE_SURCHARGE[zone]


def legacy_flat_rate(service: str, quantity: float) -> float:
    """DEPRECATED: the 2019 flat roaming rate, kept for old contracts (all migrated in 2025)."""
    flat = {"voice": 0.35, "sms": 0.11, "data": 0.45}
    return round(flat[service] * quantity, 2)


''')
rates = {}
for code, name, zone in COUNTRIES:
    for svc in ("voice", "sms", "data"):
        rates[(code, svc)] = rng.choice(ZONE_RATES[zone][svc])
    if code == "no":
        rates[(code, "voice")] = 0.12
        rates[(code, "data")] = 0.12
        rates[(code, "sms")] = 0.05
    if code == "li":
        rates[(code, "voice")] = 0.12
        rates[(code, "data")] = 0.12
    for svc in ("voice_in", "mms", "video"):
        rates[(code, svc)] = {"voice_in": round(rates[(code, "voice")] * 0.5, 2), "mms": round(rates[(code, "sms")] * 3, 2),
                              "video": round(rates[(code, "voice")] * 2 + rates[(code, "data")] * 0.5, 2)}[svc]
    if code == "no":
        rates[(code, "voice_in")] = 0.12
    for svc in SERVICES:
        r = rates[(code, svc)]
        if svc == "voice":
            w(f'''def voice_{code}(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """{name} ({zone}): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["{code}"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * {r:.2f}
    return _apply_plan(price, plan_type, plan_name)


''')
        elif svc == "sms":
            w(f'''def sms_{code}(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """{name} ({zone}): text messages, per message sent."""
    zone = COUNTRIES["{code}"]
    messages = _bill_sms(count)
    price = messages * {r:.2f}
    if zone == "ROW2" and messages > 100:
        price = 100 * {r:.2f} + (messages - 100) * {r:.2f} * 0.8
    return _apply_plan(price, plan_type, plan_name)


''')
        elif svc == "voice_in":
            w(f'''def voice_in_{code}(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """{name} ({zone}): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["{code}"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * {r:.2f}
    return _apply_plan(price, plan_type, plan_name)


''')
        elif svc == "mms":
            w(f'''def mms_{code}(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """{name} ({zone}): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * {r:.2f}
    return _apply_plan(price, plan_type, plan_name)


''')
        elif svc == "video":
            w(f'''def video_{code}(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """{name} ({zone}): video calls, per started second, at {r:.2f} a minute."""
    zone = COUNTRIES["{code}"]
    billed = _bill_seconds(seconds)
    price = billed * {r:.2f} / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


''')
        else:
            w(f'''def data_{code}(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """{name} ({zone}): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["{code}"]
    billable = _bill_mb(mb)
    price = billable * {r:.2f} + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


''')
w("RULES = {\n")
for code, name, zone in COUNTRIES:
    for svc in SERVICES:
        w(f'    ("{code}", "{svc}"): {svc}_{code},\n')
w('''}


def rate_event(country: str, service: str, quantity: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """The price of one usage event; ValueError for unknown countries, services or plans."""
    if plan_name == "legacy":
        return legacy_flat_rate(service, quantity)
    plan_info(plan_type, plan_name)
    rule = RULES.get((country, service))
    if rule is None:
        raise ValueError(f"no tariff for {service} in {country}")
    return rule(quantity, plan_type, plan_name)


def plan_label(plan_type: str, plan_name: str) -> str:
    """What the bill shows for the plan, e.g. "Business Enterprise"."""
    return f"{plan_type.capitalize()} {plan_info(plan_type, plan_name)['label']}"
''')
open("files/rating.py", "w").write("".join(out))
print(len("".join(out).splitlines()), "lines")
