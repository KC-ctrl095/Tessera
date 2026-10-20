"""Amounts are integer cents everywhere; these helpers format and split them."""

from decimal import ROUND_HALF_UP, Decimal


def format_eur(cents: int) -> str:
    """1234567 -> '€12,345.67'; negative amounts get a leading minus."""
    sign = "-" if cents < 0 else ""
    cents = abs(cents)
    return f"{sign}€{cents // 100:,}.{cents % 100:02d}"


def cents_from_text(text: str) -> int:
    """'12.50' or '12,50' -> 1250."""
    value = Decimal(text.strip().replace(",", "."))
    return int((value * 100).quantize(Decimal(1), rounding=ROUND_HALF_UP))


def split_vat(gross_cents: int, rate_percent: int) -> tuple:
    """(net, vat) of a gross amount that includes VAT at `rate_percent`."""
    net = (Decimal(gross_cents) * 100 / (100 + rate_percent)).quantize(Decimal(1), rounding=ROUND_HALF_UP)
    return int(net), gross_cents - int(net)


def share(cents: int, part: int, whole: int) -> int:
    """`cents` * part / whole, rounded half up (used for proration)."""
    if whole <= 0:
        raise ValueError("whole must be positive")
    return int((Decimal(cents) * part / whole).quantize(Decimal(1), rounding=ROUND_HALF_UP))
