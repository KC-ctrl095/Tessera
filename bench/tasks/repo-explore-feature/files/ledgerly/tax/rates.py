"""VAT rates (percent) by country and category. Only countries we invoice from."""

from decimal import Decimal

RATES = {
    "PT": {"standard": Decimal("23"), "intermediate": Decimal("13"), "reduced": Decimal("6"), "exempt": Decimal("0")},
    "ES": {"standard": Decimal("21"), "intermediate": Decimal("10"), "reduced": Decimal("4"), "exempt": Decimal("0")},
    "FR": {"standard": Decimal("20"), "intermediate": Decimal("10"), "reduced": Decimal("5.5"), "exempt": Decimal("0")},
    "DE": {"standard": Decimal("19"), "intermediate": Decimal("7"), "reduced": Decimal("7"), "exempt": Decimal("0")},
}


def rate(country: str, category: str) -> Decimal:
    try:
        return RATES[country][category]
    except KeyError:
        raise ValueError(f"no VAT rate for {category} in {country}") from None
