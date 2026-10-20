"""Customer-specific prices (volume discounts)."""

from decimal import Decimal

from ..money import to_money

VOLUME_DISCOUNTS = [(100, Decimal("0.10")), (20, Decimal("0.05"))]


def unit_price(product, quantity: int) -> Decimal:
    for threshold, discount in VOLUME_DISCOUNTS:
        if quantity >= threshold:
            return to_money(product.unit_price * (1 - discount))
    return to_money(product.unit_price)
