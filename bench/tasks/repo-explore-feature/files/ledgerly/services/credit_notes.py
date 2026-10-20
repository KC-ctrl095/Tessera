"""Credit notes cancel all or part of an invoice."""

from decimal import Decimal


def credit_amount(invoice, lines=None):
    """Gross amount credited: the whole invoice, or the given line indexes."""
    chosen = invoice.lines if lines is None else [invoice.lines[i] for i in lines]
    return sum((l.gross for l in chosen), Decimal("0.00"))
