"""Roaming rating engine: what a customer pays for calls, texts and data abroad.

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
    "ad": "ROW1",  # Andorra
    "al": "ROW1",  # Albania
    "ar": "ROW2",  # Argentina
    "at": "EU",  # Austria
    "au": "ROW2",  # Australia
    "ba": "ROW1",  # Bosnia and Herzegovina
    "be": "EU",  # Belgium
    "bg": "EU",  # Bulgaria
    "br": "ROW2",  # Brazil
    "ca": "ROW2",  # Canada
    "ch": "ROW1",  # Switzerland
    "cl": "ROW2",  # Chile
    "cn": "ROW2",  # China
    "co": "ROW2",  # Colombia
    "cy": "EU",  # Cyprus
    "cz": "EU",  # Czechia
    "de": "EU",  # Germany
    "dk": "EU",  # Denmark
    "ee": "EU",  # Estonia
    "eg": "ROW2",  # Egypt
    "es": "EU",  # Spain
    "fi": "EU",  # Finland
    "fr": "EU",  # France
    "gb": "ROW1",  # United Kingdom
    "ge": "ROW1",  # Georgia
    "gr": "EU",  # Greece
    "hr": "EU",  # Croatia
    "hu": "EU",  # Hungary
    "id": "ROW2",  # Indonesia
    "ie": "EU",  # Ireland
    "il": "ROW2",  # Israel
    "in": "ROW2",  # India
    "it": "EU",  # Italy
    "jp": "ROW2",  # Japan
    "ke": "ROW2",  # Kenya
    "kr": "ROW2",  # South Korea
    "li": "EEA",  # Liechtenstein
    "lt": "EU",  # Lithuania
    "lu": "EU",  # Luxembourg
    "lv": "EU",  # Latvia
    "ma": "ROW1",  # Morocco
    "md": "ROW1",  # Moldova
    "me": "ROW1",  # Montenegro
    "mk": "ROW1",  # North Macedonia
    "mt": "EU",  # Malta
    "mx": "ROW2",  # Mexico
    "nl": "EU",  # Netherlands
    "no": "EEA",  # Norway
    "nz": "ROW2",  # New Zealand
    "pl": "EU",  # Poland
    "pt": "EU",  # Portugal
    "ro": "EU",  # Romania
    "rs": "ROW1",  # Serbia
    "se": "EU",  # Sweden
    "si": "EU",  # Slovenia
    "sk": "EU",  # Slovakia
    "th": "ROW2",  # Thailand
    "tn": "ROW1",  # Tunisia
    "tr": "ROW1",  # Turkey
    "ua": "ROW1",  # Ukraine
    "us": "ROW2",  # United States
    "za": "ROW2",  # South Africa
}

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


def voice_ad(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Andorra (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["ad"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_ad(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Andorra (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["ad"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.24
    return _apply_plan(price, plan_type, plan_name)


def sms_ad(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Andorra (ROW1): text messages, per message sent."""
    zone = COUNTRIES["ad"]
    messages = _bill_sms(count)
    price = messages * 0.19
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.19 + (messages - 100) * 0.19 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_ad(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Andorra (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.57
    return _apply_plan(price, plan_type, plan_name)


def data_ad(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Andorra (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["ad"]
    billable = _bill_mb(mb)
    price = billable * 1.49 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_ad(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Andorra (ROW1): video calls, per started second, at 1.73 a minute."""
    zone = COUNTRIES["ad"]
    billed = _bill_seconds(seconds)
    price = billed * 1.73 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_al(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Albania (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["al"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.59
    return _apply_plan(price, plan_type, plan_name)


def voice_in_al(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Albania (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["al"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.29
    return _apply_plan(price, plan_type, plan_name)


def sms_al(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Albania (ROW1): text messages, per message sent."""
    zone = COUNTRIES["al"]
    messages = _bill_sms(count)
    price = messages * 0.25
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.25 + (messages - 100) * 0.25 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_al(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Albania (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.75
    return _apply_plan(price, plan_type, plan_name)


def data_al(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Albania (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["al"]
    billable = _bill_mb(mb)
    price = billable * 1.49 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_al(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Albania (ROW1): video calls, per started second, at 1.92 a minute."""
    zone = COUNTRIES["al"]
    billed = _bill_seconds(seconds)
    price = billed * 1.92 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_ar(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Argentina (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["ar"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.29
    return _apply_plan(price, plan_type, plan_name)


def voice_in_ar(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Argentina (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["ar"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.65
    return _apply_plan(price, plan_type, plan_name)


def sms_ar(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Argentina (ROW2): text messages, per message sent."""
    zone = COUNTRIES["ar"]
    messages = _bill_sms(count)
    price = messages * 0.29
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.29 + (messages - 100) * 0.29 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_ar(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Argentina (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.87
    return _apply_plan(price, plan_type, plan_name)


def data_ar(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Argentina (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["ar"]
    billable = _bill_mb(mb)
    price = billable * 4.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_ar(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Argentina (ROW2): video calls, per started second, at 5.08 a minute."""
    zone = COUNTRIES["ar"]
    billed = _bill_seconds(seconds)
    price = billed * 5.08 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_at(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Austria (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["at"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_at(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Austria (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["at"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_at(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Austria (EU): text messages, per message sent."""
    zone = COUNTRIES["at"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_at(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Austria (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_at(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Austria (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["at"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_at(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Austria (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["at"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_au(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Australia (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["au"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.29
    return _apply_plan(price, plan_type, plan_name)


def voice_in_au(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Australia (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["au"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.65
    return _apply_plan(price, plan_type, plan_name)


def sms_au(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Australia (ROW2): text messages, per message sent."""
    zone = COUNTRIES["au"]
    messages = _bill_sms(count)
    price = messages * 0.29
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.29 + (messages - 100) * 0.29 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_au(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Australia (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.87
    return _apply_plan(price, plan_type, plan_name)


def data_au(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Australia (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["au"]
    billable = _bill_mb(mb)
    price = billable * 5.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_au(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Australia (ROW2): video calls, per started second, at 5.58 a minute."""
    zone = COUNTRIES["au"]
    billed = _bill_seconds(seconds)
    price = billed * 5.58 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_ba(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Bosnia and Herzegovina (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["ba"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.59
    return _apply_plan(price, plan_type, plan_name)


def voice_in_ba(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Bosnia and Herzegovina (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["ba"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.29
    return _apply_plan(price, plan_type, plan_name)


def sms_ba(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Bosnia and Herzegovina (ROW1): text messages, per message sent."""
    zone = COUNTRIES["ba"]
    messages = _bill_sms(count)
    price = messages * 0.25
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.25 + (messages - 100) * 0.25 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_ba(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Bosnia and Herzegovina (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.75
    return _apply_plan(price, plan_type, plan_name)


def data_ba(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Bosnia and Herzegovina (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["ba"]
    billable = _bill_mb(mb)
    price = billable * 1.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_ba(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Bosnia and Herzegovina (ROW1): video calls, per started second, at 2.17 a minute."""
    zone = COUNTRIES["ba"]
    billed = _bill_seconds(seconds)
    price = billed * 2.17 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_be(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Belgium (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["be"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_be(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Belgium (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["be"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_be(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Belgium (EU): text messages, per message sent."""
    zone = COUNTRIES["be"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_be(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Belgium (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_be(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Belgium (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["be"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_be(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Belgium (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["be"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_bg(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Bulgaria (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["bg"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_bg(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Bulgaria (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["bg"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_bg(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Bulgaria (EU): text messages, per message sent."""
    zone = COUNTRIES["bg"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_bg(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Bulgaria (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_bg(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Bulgaria (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["bg"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_bg(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Bulgaria (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["bg"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_br(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Brazil (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["br"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.99
    return _apply_plan(price, plan_type, plan_name)


def voice_in_br(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Brazil (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["br"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.99
    return _apply_plan(price, plan_type, plan_name)


def sms_br(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Brazil (ROW2): text messages, per message sent."""
    zone = COUNTRIES["br"]
    messages = _bill_sms(count)
    price = messages * 0.29
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.29 + (messages - 100) * 0.29 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_br(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Brazil (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.87
    return _apply_plan(price, plan_type, plan_name)


def data_br(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Brazil (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["br"]
    billable = _bill_mb(mb)
    price = billable * 5.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_br(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Brazil (ROW2): video calls, per started second, at 6.97 a minute."""
    zone = COUNTRIES["br"]
    billed = _bill_seconds(seconds)
    price = billed * 6.97 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_ca(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Canada (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["ca"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.99
    return _apply_plan(price, plan_type, plan_name)


def voice_in_ca(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Canada (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["ca"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.99
    return _apply_plan(price, plan_type, plan_name)


def sms_ca(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Canada (ROW2): text messages, per message sent."""
    zone = COUNTRIES["ca"]
    messages = _bill_sms(count)
    price = messages * 0.29
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.29 + (messages - 100) * 0.29 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_ca(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Canada (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.87
    return _apply_plan(price, plan_type, plan_name)


def data_ca(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Canada (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["ca"]
    billable = _bill_mb(mb)
    price = billable * 5.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_ca(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Canada (ROW2): video calls, per started second, at 6.97 a minute."""
    zone = COUNTRIES["ca"]
    billed = _bill_seconds(seconds)
    price = billed * 6.97 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_ch(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Switzerland (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["ch"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_ch(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Switzerland (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["ch"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.24
    return _apply_plan(price, plan_type, plan_name)


def sms_ch(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Switzerland (ROW1): text messages, per message sent."""
    zone = COUNTRIES["ch"]
    messages = _bill_sms(count)
    price = messages * 0.19
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.19 + (messages - 100) * 0.19 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_ch(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Switzerland (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.57
    return _apply_plan(price, plan_type, plan_name)


def data_ch(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Switzerland (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["ch"]
    billable = _bill_mb(mb)
    price = billable * 1.49 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_ch(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Switzerland (ROW1): video calls, per started second, at 1.73 a minute."""
    zone = COUNTRIES["ch"]
    billed = _bill_seconds(seconds)
    price = billed * 1.73 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_cl(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Chile (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["cl"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.29
    return _apply_plan(price, plan_type, plan_name)


def voice_in_cl(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Chile (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["cl"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.65
    return _apply_plan(price, plan_type, plan_name)


def sms_cl(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Chile (ROW2): text messages, per message sent."""
    zone = COUNTRIES["cl"]
    messages = _bill_sms(count)
    price = messages * 0.39
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.39 + (messages - 100) * 0.39 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_cl(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Chile (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 1.17
    return _apply_plan(price, plan_type, plan_name)


def data_cl(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Chile (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["cl"]
    billable = _bill_mb(mb)
    price = billable * 6.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_cl(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Chile (ROW2): video calls, per started second, at 6.08 a minute."""
    zone = COUNTRIES["cl"]
    billed = _bill_seconds(seconds)
    price = billed * 6.08 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_cn(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """China (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["cn"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_cn(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """China (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["cn"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.74
    return _apply_plan(price, plan_type, plan_name)


def sms_cn(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """China (ROW2): text messages, per message sent."""
    zone = COUNTRIES["cn"]
    messages = _bill_sms(count)
    price = messages * 0.29
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.29 + (messages - 100) * 0.29 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_cn(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """China (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.87
    return _apply_plan(price, plan_type, plan_name)


def data_cn(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """China (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["cn"]
    billable = _bill_mb(mb)
    price = billable * 4.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_cn(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """China (ROW2): video calls, per started second, at 5.47 a minute."""
    zone = COUNTRIES["cn"]
    billed = _bill_seconds(seconds)
    price = billed * 5.47 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_co(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Colombia (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["co"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_co(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Colombia (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["co"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.74
    return _apply_plan(price, plan_type, plan_name)


def sms_co(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Colombia (ROW2): text messages, per message sent."""
    zone = COUNTRIES["co"]
    messages = _bill_sms(count)
    price = messages * 0.39
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.39 + (messages - 100) * 0.39 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_co(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Colombia (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 1.17
    return _apply_plan(price, plan_type, plan_name)


def data_co(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Colombia (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["co"]
    billable = _bill_mb(mb)
    price = billable * 4.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_co(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Colombia (ROW2): video calls, per started second, at 5.47 a minute."""
    zone = COUNTRIES["co"]
    billed = _bill_seconds(seconds)
    price = billed * 5.47 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_cy(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Cyprus (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["cy"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_cy(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Cyprus (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["cy"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_cy(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Cyprus (EU): text messages, per message sent."""
    zone = COUNTRIES["cy"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_cy(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Cyprus (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_cy(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Cyprus (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["cy"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_cy(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Cyprus (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["cy"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_cz(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Czechia (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["cz"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_cz(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Czechia (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["cz"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_cz(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Czechia (EU): text messages, per message sent."""
    zone = COUNTRIES["cz"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_cz(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Czechia (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_cz(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Czechia (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["cz"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_cz(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Czechia (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["cz"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_de(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Germany (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["de"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_de(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Germany (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["de"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_de(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Germany (EU): text messages, per message sent."""
    zone = COUNTRIES["de"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_de(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Germany (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_de(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Germany (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["de"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_de(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Germany (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["de"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_dk(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Denmark (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["dk"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_dk(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Denmark (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["dk"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_dk(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Denmark (EU): text messages, per message sent."""
    zone = COUNTRIES["dk"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_dk(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Denmark (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_dk(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Denmark (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["dk"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_dk(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Denmark (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["dk"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_ee(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Estonia (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["ee"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_ee(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Estonia (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["ee"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_ee(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Estonia (EU): text messages, per message sent."""
    zone = COUNTRIES["ee"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_ee(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Estonia (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_ee(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Estonia (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["ee"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_ee(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Estonia (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["ee"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_eg(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Egypt (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["eg"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_eg(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Egypt (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["eg"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.74
    return _apply_plan(price, plan_type, plan_name)


def sms_eg(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Egypt (ROW2): text messages, per message sent."""
    zone = COUNTRIES["eg"]
    messages = _bill_sms(count)
    price = messages * 0.39
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.39 + (messages - 100) * 0.39 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_eg(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Egypt (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 1.17
    return _apply_plan(price, plan_type, plan_name)


def data_eg(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Egypt (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["eg"]
    billable = _bill_mb(mb)
    price = billable * 4.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_eg(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Egypt (ROW2): video calls, per started second, at 5.47 a minute."""
    zone = COUNTRIES["eg"]
    billed = _bill_seconds(seconds)
    price = billed * 5.47 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_es(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Spain (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["es"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_es(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Spain (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["es"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_es(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Spain (EU): text messages, per message sent."""
    zone = COUNTRIES["es"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_es(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Spain (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_es(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Spain (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["es"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_es(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Spain (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["es"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_fi(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Finland (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["fi"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_fi(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Finland (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["fi"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_fi(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Finland (EU): text messages, per message sent."""
    zone = COUNTRIES["fi"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_fi(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Finland (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_fi(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Finland (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["fi"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_fi(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Finland (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["fi"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_fr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """France (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["fr"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_fr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """France (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["fr"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_fr(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """France (EU): text messages, per message sent."""
    zone = COUNTRIES["fr"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_fr(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """France (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_fr(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """France (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["fr"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_fr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """France (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["fr"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_gb(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """United Kingdom (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["gb"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_gb(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """United Kingdom (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["gb"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.24
    return _apply_plan(price, plan_type, plan_name)


def sms_gb(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """United Kingdom (ROW1): text messages, per message sent."""
    zone = COUNTRIES["gb"]
    messages = _bill_sms(count)
    price = messages * 0.19
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.19 + (messages - 100) * 0.19 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_gb(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """United Kingdom (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.57
    return _apply_plan(price, plan_type, plan_name)


def data_gb(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """United Kingdom (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["gb"]
    billable = _bill_mb(mb)
    price = billable * 1.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_gb(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """United Kingdom (ROW1): video calls, per started second, at 1.98 a minute."""
    zone = COUNTRIES["gb"]
    billed = _bill_seconds(seconds)
    price = billed * 1.98 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_ge(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Georgia (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["ge"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.59
    return _apply_plan(price, plan_type, plan_name)


def voice_in_ge(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Georgia (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["ge"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.29
    return _apply_plan(price, plan_type, plan_name)


def sms_ge(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Georgia (ROW1): text messages, per message sent."""
    zone = COUNTRIES["ge"]
    messages = _bill_sms(count)
    price = messages * 0.25
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.25 + (messages - 100) * 0.25 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_ge(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Georgia (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.75
    return _apply_plan(price, plan_type, plan_name)


def data_ge(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Georgia (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["ge"]
    billable = _bill_mb(mb)
    price = billable * 1.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_ge(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Georgia (ROW1): video calls, per started second, at 2.17 a minute."""
    zone = COUNTRIES["ge"]
    billed = _bill_seconds(seconds)
    price = billed * 2.17 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_gr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Greece (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["gr"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_gr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Greece (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["gr"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_gr(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Greece (EU): text messages, per message sent."""
    zone = COUNTRIES["gr"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_gr(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Greece (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_gr(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Greece (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["gr"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_gr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Greece (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["gr"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_hr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Croatia (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["hr"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_hr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Croatia (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["hr"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_hr(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Croatia (EU): text messages, per message sent."""
    zone = COUNTRIES["hr"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_hr(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Croatia (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_hr(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Croatia (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["hr"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_hr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Croatia (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["hr"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_hu(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Hungary (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["hu"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_hu(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Hungary (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["hu"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_hu(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Hungary (EU): text messages, per message sent."""
    zone = COUNTRIES["hu"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_hu(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Hungary (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_hu(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Hungary (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["hu"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_hu(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Hungary (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["hu"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_id(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Indonesia (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["id"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.99
    return _apply_plan(price, plan_type, plan_name)


def voice_in_id(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Indonesia (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["id"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.99
    return _apply_plan(price, plan_type, plan_name)


def sms_id(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Indonesia (ROW2): text messages, per message sent."""
    zone = COUNTRIES["id"]
    messages = _bill_sms(count)
    price = messages * 0.39
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.39 + (messages - 100) * 0.39 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_id(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Indonesia (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 1.17
    return _apply_plan(price, plan_type, plan_name)


def data_id(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Indonesia (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["id"]
    billable = _bill_mb(mb)
    price = billable * 5.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_id(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Indonesia (ROW2): video calls, per started second, at 6.97 a minute."""
    zone = COUNTRIES["id"]
    billed = _bill_seconds(seconds)
    price = billed * 6.97 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_ie(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Ireland (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["ie"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_ie(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Ireland (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["ie"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_ie(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Ireland (EU): text messages, per message sent."""
    zone = COUNTRIES["ie"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_ie(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Ireland (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_ie(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Ireland (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["ie"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_ie(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Ireland (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["ie"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_il(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Israel (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["il"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_il(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Israel (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["il"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.74
    return _apply_plan(price, plan_type, plan_name)


def sms_il(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Israel (ROW2): text messages, per message sent."""
    zone = COUNTRIES["il"]
    messages = _bill_sms(count)
    price = messages * 0.29
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.29 + (messages - 100) * 0.29 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_il(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Israel (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.87
    return _apply_plan(price, plan_type, plan_name)


def data_il(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Israel (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["il"]
    billable = _bill_mb(mb)
    price = billable * 6.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_il(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Israel (ROW2): video calls, per started second, at 6.47 a minute."""
    zone = COUNTRIES["il"]
    billed = _bill_seconds(seconds)
    price = billed * 6.47 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_in(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """India (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["in"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_in(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """India (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["in"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.74
    return _apply_plan(price, plan_type, plan_name)


def sms_in(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """India (ROW2): text messages, per message sent."""
    zone = COUNTRIES["in"]
    messages = _bill_sms(count)
    price = messages * 0.29
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.29 + (messages - 100) * 0.29 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_in(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """India (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.87
    return _apply_plan(price, plan_type, plan_name)


def data_in(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """India (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["in"]
    billable = _bill_mb(mb)
    price = billable * 5.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_in(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """India (ROW2): video calls, per started second, at 5.97 a minute."""
    zone = COUNTRIES["in"]
    billed = _bill_seconds(seconds)
    price = billed * 5.97 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_it(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Italy (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["it"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_it(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Italy (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["it"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_it(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Italy (EU): text messages, per message sent."""
    zone = COUNTRIES["it"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_it(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Italy (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_it(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Italy (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["it"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_it(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Italy (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["it"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_jp(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Japan (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["jp"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.29
    return _apply_plan(price, plan_type, plan_name)


def voice_in_jp(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Japan (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["jp"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.65
    return _apply_plan(price, plan_type, plan_name)


def sms_jp(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Japan (ROW2): text messages, per message sent."""
    zone = COUNTRIES["jp"]
    messages = _bill_sms(count)
    price = messages * 0.39
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.39 + (messages - 100) * 0.39 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_jp(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Japan (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 1.17
    return _apply_plan(price, plan_type, plan_name)


def data_jp(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Japan (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["jp"]
    billable = _bill_mb(mb)
    price = billable * 4.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_jp(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Japan (ROW2): video calls, per started second, at 5.08 a minute."""
    zone = COUNTRIES["jp"]
    billed = _bill_seconds(seconds)
    price = billed * 5.08 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_ke(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Kenya (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["ke"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.99
    return _apply_plan(price, plan_type, plan_name)


def voice_in_ke(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Kenya (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["ke"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.99
    return _apply_plan(price, plan_type, plan_name)


def sms_ke(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Kenya (ROW2): text messages, per message sent."""
    zone = COUNTRIES["ke"]
    messages = _bill_sms(count)
    price = messages * 0.29
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.29 + (messages - 100) * 0.29 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_ke(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Kenya (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.87
    return _apply_plan(price, plan_type, plan_name)


def data_ke(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Kenya (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["ke"]
    billable = _bill_mb(mb)
    price = billable * 6.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_ke(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Kenya (ROW2): video calls, per started second, at 7.47 a minute."""
    zone = COUNTRIES["ke"]
    billed = _bill_seconds(seconds)
    price = billed * 7.47 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_kr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """South Korea (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["kr"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_kr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """South Korea (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["kr"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.74
    return _apply_plan(price, plan_type, plan_name)


def sms_kr(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """South Korea (ROW2): text messages, per message sent."""
    zone = COUNTRIES["kr"]
    messages = _bill_sms(count)
    price = messages * 0.29
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.29 + (messages - 100) * 0.29 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_kr(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """South Korea (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.87
    return _apply_plan(price, plan_type, plan_name)


def data_kr(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """South Korea (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["kr"]
    billable = _bill_mb(mb)
    price = billable * 6.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_kr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """South Korea (ROW2): video calls, per started second, at 6.47 a minute."""
    zone = COUNTRIES["kr"]
    billed = _bill_seconds(seconds)
    price = billed * 6.47 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_li(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Liechtenstein (EEA): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["li"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.12
    return _apply_plan(price, plan_type, plan_name)


def voice_in_li(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Liechtenstein (EEA): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["li"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.06
    return _apply_plan(price, plan_type, plan_name)


def sms_li(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Liechtenstein (EEA): text messages, per message sent."""
    zone = COUNTRIES["li"]
    messages = _bill_sms(count)
    price = messages * 0.06
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.06 + (messages - 100) * 0.06 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_li(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Liechtenstein (EEA): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.18
    return _apply_plan(price, plan_type, plan_name)


def data_li(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Liechtenstein (EEA): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["li"]
    billable = _bill_mb(mb)
    price = billable * 0.12 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_li(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Liechtenstein (EEA): video calls, per started second, at 0.30 a minute."""
    zone = COUNTRIES["li"]
    billed = _bill_seconds(seconds)
    price = billed * 0.30 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_lt(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Lithuania (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["lt"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_lt(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Lithuania (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["lt"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_lt(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Lithuania (EU): text messages, per message sent."""
    zone = COUNTRIES["lt"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_lt(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Lithuania (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_lt(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Lithuania (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["lt"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_lt(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Lithuania (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["lt"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_lu(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Luxembourg (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["lu"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_lu(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Luxembourg (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["lu"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_lu(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Luxembourg (EU): text messages, per message sent."""
    zone = COUNTRIES["lu"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_lu(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Luxembourg (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_lu(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Luxembourg (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["lu"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_lu(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Luxembourg (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["lu"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_lv(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Latvia (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["lv"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_lv(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Latvia (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["lv"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_lv(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Latvia (EU): text messages, per message sent."""
    zone = COUNTRIES["lv"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_lv(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Latvia (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_lv(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Latvia (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["lv"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_lv(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Latvia (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["lv"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_ma(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Morocco (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["ma"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_ma(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Morocco (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["ma"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.24
    return _apply_plan(price, plan_type, plan_name)


def sms_ma(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Morocco (ROW1): text messages, per message sent."""
    zone = COUNTRIES["ma"]
    messages = _bill_sms(count)
    price = messages * 0.25
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.25 + (messages - 100) * 0.25 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_ma(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Morocco (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.75
    return _apply_plan(price, plan_type, plan_name)


def data_ma(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Morocco (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["ma"]
    billable = _bill_mb(mb)
    price = billable * 1.49 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_ma(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Morocco (ROW1): video calls, per started second, at 1.73 a minute."""
    zone = COUNTRIES["ma"]
    billed = _bill_seconds(seconds)
    price = billed * 1.73 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_md(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Moldova (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["md"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.59
    return _apply_plan(price, plan_type, plan_name)


def voice_in_md(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Moldova (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["md"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.29
    return _apply_plan(price, plan_type, plan_name)


def sms_md(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Moldova (ROW1): text messages, per message sent."""
    zone = COUNTRIES["md"]
    messages = _bill_sms(count)
    price = messages * 0.19
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.19 + (messages - 100) * 0.19 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_md(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Moldova (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.57
    return _apply_plan(price, plan_type, plan_name)


def data_md(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Moldova (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["md"]
    billable = _bill_mb(mb)
    price = billable * 1.49 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_md(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Moldova (ROW1): video calls, per started second, at 1.92 a minute."""
    zone = COUNTRIES["md"]
    billed = _bill_seconds(seconds)
    price = billed * 1.92 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_me(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Montenegro (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["me"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_me(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Montenegro (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["me"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.24
    return _apply_plan(price, plan_type, plan_name)


def sms_me(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Montenegro (ROW1): text messages, per message sent."""
    zone = COUNTRIES["me"]
    messages = _bill_sms(count)
    price = messages * 0.25
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.25 + (messages - 100) * 0.25 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_me(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Montenegro (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.75
    return _apply_plan(price, plan_type, plan_name)


def data_me(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Montenegro (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["me"]
    billable = _bill_mb(mb)
    price = billable * 1.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_me(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Montenegro (ROW1): video calls, per started second, at 1.98 a minute."""
    zone = COUNTRIES["me"]
    billed = _bill_seconds(seconds)
    price = billed * 1.98 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_mk(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """North Macedonia (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["mk"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_mk(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """North Macedonia (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["mk"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.24
    return _apply_plan(price, plan_type, plan_name)


def sms_mk(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """North Macedonia (ROW1): text messages, per message sent."""
    zone = COUNTRIES["mk"]
    messages = _bill_sms(count)
    price = messages * 0.25
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.25 + (messages - 100) * 0.25 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_mk(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """North Macedonia (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.75
    return _apply_plan(price, plan_type, plan_name)


def data_mk(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """North Macedonia (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["mk"]
    billable = _bill_mb(mb)
    price = billable * 1.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_mk(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """North Macedonia (ROW1): video calls, per started second, at 1.98 a minute."""
    zone = COUNTRIES["mk"]
    billed = _bill_seconds(seconds)
    price = billed * 1.98 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_mt(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Malta (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["mt"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_mt(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Malta (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["mt"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_mt(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Malta (EU): text messages, per message sent."""
    zone = COUNTRIES["mt"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_mt(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Malta (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_mt(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Malta (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["mt"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_mt(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Malta (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["mt"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_mx(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Mexico (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["mx"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_mx(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Mexico (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["mx"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.74
    return _apply_plan(price, plan_type, plan_name)


def sms_mx(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Mexico (ROW2): text messages, per message sent."""
    zone = COUNTRIES["mx"]
    messages = _bill_sms(count)
    price = messages * 0.39
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.39 + (messages - 100) * 0.39 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_mx(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Mexico (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 1.17
    return _apply_plan(price, plan_type, plan_name)


def data_mx(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Mexico (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["mx"]
    billable = _bill_mb(mb)
    price = billable * 6.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_mx(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Mexico (ROW2): video calls, per started second, at 6.47 a minute."""
    zone = COUNTRIES["mx"]
    billed = _bill_seconds(seconds)
    price = billed * 6.47 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_nl(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Netherlands (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["nl"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_nl(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Netherlands (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["nl"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_nl(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Netherlands (EU): text messages, per message sent."""
    zone = COUNTRIES["nl"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_nl(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Netherlands (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_nl(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Netherlands (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["nl"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_nl(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Netherlands (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["nl"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_no(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Norway (EEA): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["no"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.12
    return _apply_plan(price, plan_type, plan_name)


def voice_in_no(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Norway (EEA): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["no"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.12
    return _apply_plan(price, plan_type, plan_name)


def sms_no(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Norway (EEA): text messages, per message sent."""
    zone = COUNTRIES["no"]
    messages = _bill_sms(count)
    price = messages * 0.05
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.05 + (messages - 100) * 0.05 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_no(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Norway (EEA): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.15
    return _apply_plan(price, plan_type, plan_name)


def data_no(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Norway (EEA): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["no"]
    billable = _bill_mb(mb)
    price = billable * 0.12 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_no(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Norway (EEA): video calls, per started second, at 0.30 a minute."""
    zone = COUNTRIES["no"]
    billed = _bill_seconds(seconds)
    price = billed * 0.30 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_nz(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """New Zealand (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["nz"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.99
    return _apply_plan(price, plan_type, plan_name)


def voice_in_nz(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """New Zealand (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["nz"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.99
    return _apply_plan(price, plan_type, plan_name)


def sms_nz(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """New Zealand (ROW2): text messages, per message sent."""
    zone = COUNTRIES["nz"]
    messages = _bill_sms(count)
    price = messages * 0.39
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.39 + (messages - 100) * 0.39 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_nz(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """New Zealand (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 1.17
    return _apply_plan(price, plan_type, plan_name)


def data_nz(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """New Zealand (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["nz"]
    billable = _bill_mb(mb)
    price = billable * 5.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_nz(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """New Zealand (ROW2): video calls, per started second, at 6.97 a minute."""
    zone = COUNTRIES["nz"]
    billed = _bill_seconds(seconds)
    price = billed * 6.97 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_pl(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Poland (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["pl"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_pl(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Poland (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["pl"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_pl(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Poland (EU): text messages, per message sent."""
    zone = COUNTRIES["pl"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_pl(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Poland (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_pl(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Poland (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["pl"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_pl(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Poland (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["pl"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_pt(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Portugal (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["pt"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_pt(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Portugal (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["pt"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_pt(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Portugal (EU): text messages, per message sent."""
    zone = COUNTRIES["pt"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_pt(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Portugal (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_pt(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Portugal (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["pt"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_pt(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Portugal (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["pt"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_ro(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Romania (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["ro"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_ro(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Romania (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["ro"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_ro(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Romania (EU): text messages, per message sent."""
    zone = COUNTRIES["ro"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_ro(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Romania (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_ro(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Romania (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["ro"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_ro(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Romania (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["ro"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_rs(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Serbia (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["rs"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_rs(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Serbia (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["rs"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.24
    return _apply_plan(price, plan_type, plan_name)


def sms_rs(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Serbia (ROW1): text messages, per message sent."""
    zone = COUNTRIES["rs"]
    messages = _bill_sms(count)
    price = messages * 0.25
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.25 + (messages - 100) * 0.25 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_rs(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Serbia (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.75
    return _apply_plan(price, plan_type, plan_name)


def data_rs(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Serbia (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["rs"]
    billable = _bill_mb(mb)
    price = billable * 1.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_rs(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Serbia (ROW1): video calls, per started second, at 1.98 a minute."""
    zone = COUNTRIES["rs"]
    billed = _bill_seconds(seconds)
    price = billed * 1.98 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_se(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Sweden (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["se"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_se(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Sweden (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["se"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_se(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Sweden (EU): text messages, per message sent."""
    zone = COUNTRIES["se"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_se(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Sweden (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_se(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Sweden (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["se"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_se(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Sweden (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["se"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_si(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Slovenia (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["si"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_si(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Slovenia (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["si"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_si(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Slovenia (EU): text messages, per message sent."""
    zone = COUNTRIES["si"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_si(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Slovenia (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_si(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Slovenia (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["si"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_si(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Slovenia (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["si"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_sk(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Slovakia (EU): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["sk"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.00
    return _apply_plan(price, plan_type, plan_name)


def voice_in_sk(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Slovakia (EU): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["sk"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.00
    return _apply_plan(price, plan_type, plan_name)


def sms_sk(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Slovakia (EU): text messages, per message sent."""
    zone = COUNTRIES["sk"]
    messages = _bill_sms(count)
    price = messages * 0.00
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.00 + (messages - 100) * 0.00 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_sk(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Slovakia (EU): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.00
    return _apply_plan(price, plan_type, plan_name)


def data_sk(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Slovakia (EU): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["sk"]
    billable = _bill_mb(mb)
    price = billable * 0.00 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_sk(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Slovakia (EU): video calls, per started second, at 0.00 a minute."""
    zone = COUNTRIES["sk"]
    billed = _bill_seconds(seconds)
    price = billed * 0.00 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_th(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Thailand (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["th"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.99
    return _apply_plan(price, plan_type, plan_name)


def voice_in_th(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Thailand (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["th"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.99
    return _apply_plan(price, plan_type, plan_name)


def sms_th(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Thailand (ROW2): text messages, per message sent."""
    zone = COUNTRIES["th"]
    messages = _bill_sms(count)
    price = messages * 0.29
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.29 + (messages - 100) * 0.29 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_th(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Thailand (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.87
    return _apply_plan(price, plan_type, plan_name)


def data_th(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Thailand (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["th"]
    billable = _bill_mb(mb)
    price = billable * 4.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_th(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Thailand (ROW2): video calls, per started second, at 6.47 a minute."""
    zone = COUNTRIES["th"]
    billed = _bill_seconds(seconds)
    price = billed * 6.47 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_tn(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Tunisia (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["tn"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_tn(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Tunisia (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["tn"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.24
    return _apply_plan(price, plan_type, plan_name)


def sms_tn(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Tunisia (ROW1): text messages, per message sent."""
    zone = COUNTRIES["tn"]
    messages = _bill_sms(count)
    price = messages * 0.19
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.19 + (messages - 100) * 0.19 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_tn(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Tunisia (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.57
    return _apply_plan(price, plan_type, plan_name)


def data_tn(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Tunisia (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["tn"]
    billable = _bill_mb(mb)
    price = billable * 1.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_tn(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Tunisia (ROW1): video calls, per started second, at 1.98 a minute."""
    zone = COUNTRIES["tn"]
    billed = _bill_seconds(seconds)
    price = billed * 1.98 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_tr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Turkey (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["tr"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.49
    return _apply_plan(price, plan_type, plan_name)


def voice_in_tr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Turkey (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["tr"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.24
    return _apply_plan(price, plan_type, plan_name)


def sms_tr(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Turkey (ROW1): text messages, per message sent."""
    zone = COUNTRIES["tr"]
    messages = _bill_sms(count)
    price = messages * 0.25
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.25 + (messages - 100) * 0.25 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_tr(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Turkey (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.75
    return _apply_plan(price, plan_type, plan_name)


def data_tr(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Turkey (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["tr"]
    billable = _bill_mb(mb)
    price = billable * 1.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_tr(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Turkey (ROW1): video calls, per started second, at 1.98 a minute."""
    zone = COUNTRIES["tr"]
    billed = _bill_seconds(seconds)
    price = billed * 1.98 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_ua(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Ukraine (ROW1): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["ua"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 0.69
    return _apply_plan(price, plan_type, plan_name)


def voice_in_ua(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Ukraine (ROW1): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["ua"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.34
    return _apply_plan(price, plan_type, plan_name)


def sms_ua(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Ukraine (ROW1): text messages, per message sent."""
    zone = COUNTRIES["ua"]
    messages = _bill_sms(count)
    price = messages * 0.19
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.19 + (messages - 100) * 0.19 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_ua(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Ukraine (ROW1): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.57
    return _apply_plan(price, plan_type, plan_name)


def data_ua(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Ukraine (ROW1): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["ua"]
    billable = _bill_mb(mb)
    price = billable * 1.49 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_ua(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """Ukraine (ROW1): video calls, per started second, at 2.12 a minute."""
    zone = COUNTRIES["ua"]
    billed = _bill_seconds(seconds)
    price = billed * 2.12 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_us(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """United States (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["us"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.99
    return _apply_plan(price, plan_type, plan_name)


def voice_in_us(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """United States (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["us"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.99
    return _apply_plan(price, plan_type, plan_name)


def sms_us(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """United States (ROW2): text messages, per message sent."""
    zone = COUNTRIES["us"]
    messages = _bill_sms(count)
    price = messages * 0.29
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.29 + (messages - 100) * 0.29 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_us(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """United States (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 0.87
    return _apply_plan(price, plan_type, plan_name)


def data_us(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """United States (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["us"]
    billable = _bill_mb(mb)
    price = billable * 6.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_us(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """United States (ROW2): video calls, per started second, at 7.47 a minute."""
    zone = COUNTRIES["us"]
    billed = _bill_seconds(seconds)
    price = billed * 7.47 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


def voice_za(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """South Africa (ROW2): calls, per started minute; plan minutes are included first."""
    zone = COUNTRIES["za"]
    minutes = _bill_minutes(seconds)
    included = plan_info(plan_type, plan_name)["included_minutes"] if zone in ("EU", "EEA") else 0
    chargeable = max(0, minutes - included)
    price = chargeable * 1.99
    return _apply_plan(price, plan_type, plan_name)


def voice_in_za(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """South Africa (ROW2): incoming calls, per started minute (free in the EU and EEA)."""
    zone = COUNTRIES["za"]
    if zone in ("EU", "EEA"):
        return 0.0
    minutes = _bill_minutes(seconds)
    price = minutes * 0.99
    return _apply_plan(price, plan_type, plan_name)


def sms_za(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """South Africa (ROW2): text messages, per message sent."""
    zone = COUNTRIES["za"]
    messages = _bill_sms(count)
    price = messages * 0.39
    if zone == "ROW2" and messages > 100:
        price = 100 * 0.39 + (messages - 100) * 0.39 * 0.8
    return _apply_plan(price, plan_type, plan_name)


def mms_za(count: int, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """South Africa (ROW2): picture messages, per message sent."""
    messages = _bill_sms(count)
    price = messages * 1.17
    return _apply_plan(price, plan_type, plan_name)


def data_za(mb: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """South Africa (ROW2): mobile data, per started 0.1 MB, with the zone's fair-use surcharge."""
    zone = COUNTRIES["za"]
    billable = _bill_mb(mb)
    price = billable * 5.99 + _fair_use(zone, billable)
    return _apply_plan(price, plan_type, plan_name)


def video_za(seconds: float, plan_type: str = "personal", plan_name: str = "Basic") -> float:
    """South Africa (ROW2): video calls, per started second, at 6.97 a minute."""
    zone = COUNTRIES["za"]
    billed = _bill_seconds(seconds)
    price = billed * 6.97 / 60
    if zone in ("ROW1", "ROW2") and billed > 3600:
        price += 1.50
    return _apply_plan(price, plan_type, plan_name)


RULES = {
    ("ad", "voice"): voice_ad,
    ("ad", "voice_in"): voice_in_ad,
    ("ad", "sms"): sms_ad,
    ("ad", "mms"): mms_ad,
    ("ad", "data"): data_ad,
    ("ad", "video"): video_ad,
    ("al", "voice"): voice_al,
    ("al", "voice_in"): voice_in_al,
    ("al", "sms"): sms_al,
    ("al", "mms"): mms_al,
    ("al", "data"): data_al,
    ("al", "video"): video_al,
    ("ar", "voice"): voice_ar,
    ("ar", "voice_in"): voice_in_ar,
    ("ar", "sms"): sms_ar,
    ("ar", "mms"): mms_ar,
    ("ar", "data"): data_ar,
    ("ar", "video"): video_ar,
    ("at", "voice"): voice_at,
    ("at", "voice_in"): voice_in_at,
    ("at", "sms"): sms_at,
    ("at", "mms"): mms_at,
    ("at", "data"): data_at,
    ("at", "video"): video_at,
    ("au", "voice"): voice_au,
    ("au", "voice_in"): voice_in_au,
    ("au", "sms"): sms_au,
    ("au", "mms"): mms_au,
    ("au", "data"): data_au,
    ("au", "video"): video_au,
    ("ba", "voice"): voice_ba,
    ("ba", "voice_in"): voice_in_ba,
    ("ba", "sms"): sms_ba,
    ("ba", "mms"): mms_ba,
    ("ba", "data"): data_ba,
    ("ba", "video"): video_ba,
    ("be", "voice"): voice_be,
    ("be", "voice_in"): voice_in_be,
    ("be", "sms"): sms_be,
    ("be", "mms"): mms_be,
    ("be", "data"): data_be,
    ("be", "video"): video_be,
    ("bg", "voice"): voice_bg,
    ("bg", "voice_in"): voice_in_bg,
    ("bg", "sms"): sms_bg,
    ("bg", "mms"): mms_bg,
    ("bg", "data"): data_bg,
    ("bg", "video"): video_bg,
    ("br", "voice"): voice_br,
    ("br", "voice_in"): voice_in_br,
    ("br", "sms"): sms_br,
    ("br", "mms"): mms_br,
    ("br", "data"): data_br,
    ("br", "video"): video_br,
    ("ca", "voice"): voice_ca,
    ("ca", "voice_in"): voice_in_ca,
    ("ca", "sms"): sms_ca,
    ("ca", "mms"): mms_ca,
    ("ca", "data"): data_ca,
    ("ca", "video"): video_ca,
    ("ch", "voice"): voice_ch,
    ("ch", "voice_in"): voice_in_ch,
    ("ch", "sms"): sms_ch,
    ("ch", "mms"): mms_ch,
    ("ch", "data"): data_ch,
    ("ch", "video"): video_ch,
    ("cl", "voice"): voice_cl,
    ("cl", "voice_in"): voice_in_cl,
    ("cl", "sms"): sms_cl,
    ("cl", "mms"): mms_cl,
    ("cl", "data"): data_cl,
    ("cl", "video"): video_cl,
    ("cn", "voice"): voice_cn,
    ("cn", "voice_in"): voice_in_cn,
    ("cn", "sms"): sms_cn,
    ("cn", "mms"): mms_cn,
    ("cn", "data"): data_cn,
    ("cn", "video"): video_cn,
    ("co", "voice"): voice_co,
    ("co", "voice_in"): voice_in_co,
    ("co", "sms"): sms_co,
    ("co", "mms"): mms_co,
    ("co", "data"): data_co,
    ("co", "video"): video_co,
    ("cy", "voice"): voice_cy,
    ("cy", "voice_in"): voice_in_cy,
    ("cy", "sms"): sms_cy,
    ("cy", "mms"): mms_cy,
    ("cy", "data"): data_cy,
    ("cy", "video"): video_cy,
    ("cz", "voice"): voice_cz,
    ("cz", "voice_in"): voice_in_cz,
    ("cz", "sms"): sms_cz,
    ("cz", "mms"): mms_cz,
    ("cz", "data"): data_cz,
    ("cz", "video"): video_cz,
    ("de", "voice"): voice_de,
    ("de", "voice_in"): voice_in_de,
    ("de", "sms"): sms_de,
    ("de", "mms"): mms_de,
    ("de", "data"): data_de,
    ("de", "video"): video_de,
    ("dk", "voice"): voice_dk,
    ("dk", "voice_in"): voice_in_dk,
    ("dk", "sms"): sms_dk,
    ("dk", "mms"): mms_dk,
    ("dk", "data"): data_dk,
    ("dk", "video"): video_dk,
    ("ee", "voice"): voice_ee,
    ("ee", "voice_in"): voice_in_ee,
    ("ee", "sms"): sms_ee,
    ("ee", "mms"): mms_ee,
    ("ee", "data"): data_ee,
    ("ee", "video"): video_ee,
    ("eg", "voice"): voice_eg,
    ("eg", "voice_in"): voice_in_eg,
    ("eg", "sms"): sms_eg,
    ("eg", "mms"): mms_eg,
    ("eg", "data"): data_eg,
    ("eg", "video"): video_eg,
    ("es", "voice"): voice_es,
    ("es", "voice_in"): voice_in_es,
    ("es", "sms"): sms_es,
    ("es", "mms"): mms_es,
    ("es", "data"): data_es,
    ("es", "video"): video_es,
    ("fi", "voice"): voice_fi,
    ("fi", "voice_in"): voice_in_fi,
    ("fi", "sms"): sms_fi,
    ("fi", "mms"): mms_fi,
    ("fi", "data"): data_fi,
    ("fi", "video"): video_fi,
    ("fr", "voice"): voice_fr,
    ("fr", "voice_in"): voice_in_fr,
    ("fr", "sms"): sms_fr,
    ("fr", "mms"): mms_fr,
    ("fr", "data"): data_fr,
    ("fr", "video"): video_fr,
    ("gb", "voice"): voice_gb,
    ("gb", "voice_in"): voice_in_gb,
    ("gb", "sms"): sms_gb,
    ("gb", "mms"): mms_gb,
    ("gb", "data"): data_gb,
    ("gb", "video"): video_gb,
    ("ge", "voice"): voice_ge,
    ("ge", "voice_in"): voice_in_ge,
    ("ge", "sms"): sms_ge,
    ("ge", "mms"): mms_ge,
    ("ge", "data"): data_ge,
    ("ge", "video"): video_ge,
    ("gr", "voice"): voice_gr,
    ("gr", "voice_in"): voice_in_gr,
    ("gr", "sms"): sms_gr,
    ("gr", "mms"): mms_gr,
    ("gr", "data"): data_gr,
    ("gr", "video"): video_gr,
    ("hr", "voice"): voice_hr,
    ("hr", "voice_in"): voice_in_hr,
    ("hr", "sms"): sms_hr,
    ("hr", "mms"): mms_hr,
    ("hr", "data"): data_hr,
    ("hr", "video"): video_hr,
    ("hu", "voice"): voice_hu,
    ("hu", "voice_in"): voice_in_hu,
    ("hu", "sms"): sms_hu,
    ("hu", "mms"): mms_hu,
    ("hu", "data"): data_hu,
    ("hu", "video"): video_hu,
    ("id", "voice"): voice_id,
    ("id", "voice_in"): voice_in_id,
    ("id", "sms"): sms_id,
    ("id", "mms"): mms_id,
    ("id", "data"): data_id,
    ("id", "video"): video_id,
    ("ie", "voice"): voice_ie,
    ("ie", "voice_in"): voice_in_ie,
    ("ie", "sms"): sms_ie,
    ("ie", "mms"): mms_ie,
    ("ie", "data"): data_ie,
    ("ie", "video"): video_ie,
    ("il", "voice"): voice_il,
    ("il", "voice_in"): voice_in_il,
    ("il", "sms"): sms_il,
    ("il", "mms"): mms_il,
    ("il", "data"): data_il,
    ("il", "video"): video_il,
    ("in", "voice"): voice_in,
    ("in", "voice_in"): voice_in_in,
    ("in", "sms"): sms_in,
    ("in", "mms"): mms_in,
    ("in", "data"): data_in,
    ("in", "video"): video_in,
    ("it", "voice"): voice_it,
    ("it", "voice_in"): voice_in_it,
    ("it", "sms"): sms_it,
    ("it", "mms"): mms_it,
    ("it", "data"): data_it,
    ("it", "video"): video_it,
    ("jp", "voice"): voice_jp,
    ("jp", "voice_in"): voice_in_jp,
    ("jp", "sms"): sms_jp,
    ("jp", "mms"): mms_jp,
    ("jp", "data"): data_jp,
    ("jp", "video"): video_jp,
    ("ke", "voice"): voice_ke,
    ("ke", "voice_in"): voice_in_ke,
    ("ke", "sms"): sms_ke,
    ("ke", "mms"): mms_ke,
    ("ke", "data"): data_ke,
    ("ke", "video"): video_ke,
    ("kr", "voice"): voice_kr,
    ("kr", "voice_in"): voice_in_kr,
    ("kr", "sms"): sms_kr,
    ("kr", "mms"): mms_kr,
    ("kr", "data"): data_kr,
    ("kr", "video"): video_kr,
    ("li", "voice"): voice_li,
    ("li", "voice_in"): voice_in_li,
    ("li", "sms"): sms_li,
    ("li", "mms"): mms_li,
    ("li", "data"): data_li,
    ("li", "video"): video_li,
    ("lt", "voice"): voice_lt,
    ("lt", "voice_in"): voice_in_lt,
    ("lt", "sms"): sms_lt,
    ("lt", "mms"): mms_lt,
    ("lt", "data"): data_lt,
    ("lt", "video"): video_lt,
    ("lu", "voice"): voice_lu,
    ("lu", "voice_in"): voice_in_lu,
    ("lu", "sms"): sms_lu,
    ("lu", "mms"): mms_lu,
    ("lu", "data"): data_lu,
    ("lu", "video"): video_lu,
    ("lv", "voice"): voice_lv,
    ("lv", "voice_in"): voice_in_lv,
    ("lv", "sms"): sms_lv,
    ("lv", "mms"): mms_lv,
    ("lv", "data"): data_lv,
    ("lv", "video"): video_lv,
    ("ma", "voice"): voice_ma,
    ("ma", "voice_in"): voice_in_ma,
    ("ma", "sms"): sms_ma,
    ("ma", "mms"): mms_ma,
    ("ma", "data"): data_ma,
    ("ma", "video"): video_ma,
    ("md", "voice"): voice_md,
    ("md", "voice_in"): voice_in_md,
    ("md", "sms"): sms_md,
    ("md", "mms"): mms_md,
    ("md", "data"): data_md,
    ("md", "video"): video_md,
    ("me", "voice"): voice_me,
    ("me", "voice_in"): voice_in_me,
    ("me", "sms"): sms_me,
    ("me", "mms"): mms_me,
    ("me", "data"): data_me,
    ("me", "video"): video_me,
    ("mk", "voice"): voice_mk,
    ("mk", "voice_in"): voice_in_mk,
    ("mk", "sms"): sms_mk,
    ("mk", "mms"): mms_mk,
    ("mk", "data"): data_mk,
    ("mk", "video"): video_mk,
    ("mt", "voice"): voice_mt,
    ("mt", "voice_in"): voice_in_mt,
    ("mt", "sms"): sms_mt,
    ("mt", "mms"): mms_mt,
    ("mt", "data"): data_mt,
    ("mt", "video"): video_mt,
    ("mx", "voice"): voice_mx,
    ("mx", "voice_in"): voice_in_mx,
    ("mx", "sms"): sms_mx,
    ("mx", "mms"): mms_mx,
    ("mx", "data"): data_mx,
    ("mx", "video"): video_mx,
    ("nl", "voice"): voice_nl,
    ("nl", "voice_in"): voice_in_nl,
    ("nl", "sms"): sms_nl,
    ("nl", "mms"): mms_nl,
    ("nl", "data"): data_nl,
    ("nl", "video"): video_nl,
    ("no", "voice"): voice_no,
    ("no", "voice_in"): voice_in_no,
    ("no", "sms"): sms_no,
    ("no", "mms"): mms_no,
    ("no", "data"): data_no,
    ("no", "video"): video_no,
    ("nz", "voice"): voice_nz,
    ("nz", "voice_in"): voice_in_nz,
    ("nz", "sms"): sms_nz,
    ("nz", "mms"): mms_nz,
    ("nz", "data"): data_nz,
    ("nz", "video"): video_nz,
    ("pl", "voice"): voice_pl,
    ("pl", "voice_in"): voice_in_pl,
    ("pl", "sms"): sms_pl,
    ("pl", "mms"): mms_pl,
    ("pl", "data"): data_pl,
    ("pl", "video"): video_pl,
    ("pt", "voice"): voice_pt,
    ("pt", "voice_in"): voice_in_pt,
    ("pt", "sms"): sms_pt,
    ("pt", "mms"): mms_pt,
    ("pt", "data"): data_pt,
    ("pt", "video"): video_pt,
    ("ro", "voice"): voice_ro,
    ("ro", "voice_in"): voice_in_ro,
    ("ro", "sms"): sms_ro,
    ("ro", "mms"): mms_ro,
    ("ro", "data"): data_ro,
    ("ro", "video"): video_ro,
    ("rs", "voice"): voice_rs,
    ("rs", "voice_in"): voice_in_rs,
    ("rs", "sms"): sms_rs,
    ("rs", "mms"): mms_rs,
    ("rs", "data"): data_rs,
    ("rs", "video"): video_rs,
    ("se", "voice"): voice_se,
    ("se", "voice_in"): voice_in_se,
    ("se", "sms"): sms_se,
    ("se", "mms"): mms_se,
    ("se", "data"): data_se,
    ("se", "video"): video_se,
    ("si", "voice"): voice_si,
    ("si", "voice_in"): voice_in_si,
    ("si", "sms"): sms_si,
    ("si", "mms"): mms_si,
    ("si", "data"): data_si,
    ("si", "video"): video_si,
    ("sk", "voice"): voice_sk,
    ("sk", "voice_in"): voice_in_sk,
    ("sk", "sms"): sms_sk,
    ("sk", "mms"): mms_sk,
    ("sk", "data"): data_sk,
    ("sk", "video"): video_sk,
    ("th", "voice"): voice_th,
    ("th", "voice_in"): voice_in_th,
    ("th", "sms"): sms_th,
    ("th", "mms"): mms_th,
    ("th", "data"): data_th,
    ("th", "video"): video_th,
    ("tn", "voice"): voice_tn,
    ("tn", "voice_in"): voice_in_tn,
    ("tn", "sms"): sms_tn,
    ("tn", "mms"): mms_tn,
    ("tn", "data"): data_tn,
    ("tn", "video"): video_tn,
    ("tr", "voice"): voice_tr,
    ("tr", "voice_in"): voice_in_tr,
    ("tr", "sms"): sms_tr,
    ("tr", "mms"): mms_tr,
    ("tr", "data"): data_tr,
    ("tr", "video"): video_tr,
    ("ua", "voice"): voice_ua,
    ("ua", "voice_in"): voice_in_ua,
    ("ua", "sms"): sms_ua,
    ("ua", "mms"): mms_ua,
    ("ua", "data"): data_ua,
    ("ua", "video"): video_ua,
    ("us", "voice"): voice_us,
    ("us", "voice_in"): voice_in_us,
    ("us", "sms"): sms_us,
    ("us", "mms"): mms_us,
    ("us", "data"): data_us,
    ("us", "video"): video_us,
    ("za", "voice"): voice_za,
    ("za", "voice_in"): voice_in_za,
    ("za", "sms"): sms_za,
    ("za", "mms"): mms_za,
    ("za", "data"): data_za,
    ("za", "video"): video_za,
}


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
