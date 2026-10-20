"""Invoices for payments: number, VAT split and due date."""

from dataclasses import dataclass
from datetime import datetime

from core.money import split_vat

from .due_dates import due_at
from .numbering import Numbering
from .tax import vat_rate


@dataclass(frozen=True)
class Invoice:
    number: str
    payment_id: int
    customer_id: int
    issued_at: datetime
    due_at: datetime
    net_cents: int
    vat_cents: int
    gross_cents: int
    vat_rate: int


def invoice_for(store, payment, numbering: Numbering, issued_at=None) -> Invoice:
    """The invoice of a payment; issued when the payment was made unless told otherwise."""
    issued = issued_at or payment.paid_at
    customer = store.customer(payment.customer_id)
    rate = vat_rate(customer.country)
    net, vat = split_vat(payment.amount_cents, rate)
    return Invoice(
        number=numbering.next(issued),
        payment_id=payment.id,
        customer_id=customer.id,
        issued_at=issued,
        due_at=due_at(issued),
        net_cents=net,
        vat_cents=vat,
        gross_cents=payment.amount_cents,
        vat_rate=rate,
    )
