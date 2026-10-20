"""Monthly recurring invoices (subscriptions)."""

from datetime import date

from .invoicing import create_invoice


def run_month(subscriptions, year, month):
    issued = date(year, month, 1)
    return [create_invoice(s["customer"], s["items"], issued) for s in subscriptions if s.get("active", True)]
