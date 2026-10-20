"""Money helpers: amounts are Decimals rounded to cents, half up."""

from decimal import ROUND_HALF_UP, Decimal

CENT = Decimal("0.01")


def to_money(value) -> Decimal:
    return Decimal(str(value)).quantize(CENT, rounding=ROUND_HALF_UP)


def percent_of(amount: Decimal, rate: Decimal) -> Decimal:
    """rate is a percentage (23 means 23%)."""
    return to_money(amount * rate / 100)


def fmt(amount: Decimal, currency: str = "EUR") -> str:
    return f"{to_money(amount):,.2f} {currency}"
