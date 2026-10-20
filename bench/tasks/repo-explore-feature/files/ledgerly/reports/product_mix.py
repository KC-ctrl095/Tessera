"""Net revenue per product SKU."""

from collections import defaultdict
from decimal import Decimal


def build(invoices):
    totals = defaultdict(lambda: Decimal("0.00"))
    for inv in invoices:
        for line in inv.lines:
            totals[line.product.sku] += line.net
    return dict(sorted(totals.items()))
