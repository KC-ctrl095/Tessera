"""VAT charged per rate (for the periodic VAT return)."""

from collections import defaultdict
from decimal import Decimal


def build(invoices):
    """[(rate, net, vat)] sorted by rate, highest first."""
    net = defaultdict(lambda: Decimal("0.00"))
    vat = defaultdict(lambda: Decimal("0.00"))
    for inv in invoices:
        for line in inv.lines:
            net[line.vat_rate] += line.net
            vat[line.vat_rate] += line.vat
    return [(r, net[r], vat[r]) for r in sorted(net, reverse=True)]
