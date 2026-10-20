"""Creating invoices."""

from datetime import timedelta

from ..config import company
from ..models import Invoice, InvoiceLine
from ..tax.vat import vat_rate_for
from . import notes, numbering, pricing


def create_invoice(customer, items, issued_on):
    """An invoice for `items` ([(product, quantity), ...]) to `customer`."""
    seller = company()
    lines = [
        InvoiceLine(
            product=product,
            quantity=qty,
            unit_price=pricing.unit_price(product, qty),
            vat_rate=vat_rate_for(seller.country, customer, product),
        )
        for product, qty in items
    ]
    invoice = Invoice(
        number=numbering.next_number(issued_on.year),
        customer=customer,
        issued_on=issued_on,
        due_on=issued_on + timedelta(days=seller.payment_terms_days),
        currency=seller.currency,
        lines=lines,
    )
    invoice.notes = notes.notes_for(invoice)
    return invoice
