"""ledgerly 2.x invoice totals (replaced by services/invoicing.py in 3.0). Kept for re-printing
archived 2023 invoices; NOT used for new invoices."""

from decimal import Decimal

from ..countries import EEA

REVERSE_CHARGE_NOTE = "Reverse charge"


def v1_totals(lines, customer_country, customer_vat_id, seller_country="PT"):
    """lines: [(net, vat_rate)]. 2.x treated the whole EEA alike and trusted any VAT id."""
    reverse = customer_country != seller_country and customer_country in EEA and bool(customer_vat_id)
    net = sum((Decimal(n) for n, _ in lines), Decimal("0"))
    vat = Decimal("0") if reverse else sum((Decimal(n) * Decimal(r) / 100 for n, r in lines), Decimal("0"))
    return {"net": net, "vat": vat, "notes": [REVERSE_CHARGE_NOTE] if reverse else []}
