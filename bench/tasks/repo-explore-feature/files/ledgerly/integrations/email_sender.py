"""Sends invoices by email (the outbox is a folder the mail relay picks up)."""

from pathlib import Path

from ..rendering.text_renderer import render_invoice_text

OUTBOX = Path("outbox")


def queue_invoice_email(invoice, outbox=OUTBOX):
    outbox.mkdir(parents=True, exist_ok=True)
    path = outbox / f"{invoice.number.replace(' ', '_').replace('/', '-')}.eml"
    path.write_text(f"To: {invoice.customer.email}\nSubject: Invoice {invoice.number}\n\n{render_invoice_text(invoice)}")
    return path
