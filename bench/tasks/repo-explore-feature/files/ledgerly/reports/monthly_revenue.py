"""Gross revenue per month (YYYY-MM)."""

from collections import defaultdict
from decimal import Decimal


def build(invoices):
    totals = defaultdict(lambda: Decimal("0.00"))
    for inv in invoices:
        totals[inv.issued_on.strftime('%Y-%m')] += inv.gross_total
    return sorted(totals.items(), key=lambda kv: (-kv[1], kv[0]))
