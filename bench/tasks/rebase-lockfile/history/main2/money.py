"""Amounts: parsing and rounding (always Decimal, never float)."""

from decimal import ROUND_HALF_EVEN, ROUND_HALF_UP, Decimal, InvalidOperation

CENT = Decimal("0.01")


def parse_amount(text):
    """'12.50', '-3.335', '1,234.50' or '(12.50)' (a negative amount) as a Decimal.

    Amounts converted from other currencies can carry a third decimal.
    """
    s = text.strip().replace(",", "")
    negative = s.startswith("(") and s.endswith(")")
    if negative:
        s = s[1:-1]
    try:
        value = Decimal(s)
    except InvalidOperation:
        raise ValueError(f"not an amount: {text!r}") from None
    return -value if negative else value


def round_cents(x):
    return x.quantize(CENT, rounding=ROUND_HALF_UP)


def round_half_even(x):
    """Cents, rounded the way the bank rounds on its statements (half to even)."""
    return x.quantize(CENT, rounding=ROUND_HALF_EVEN)
