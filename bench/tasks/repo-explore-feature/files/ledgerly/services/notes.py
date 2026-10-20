"""Standard notes printed on invoices. Each note decides for itself whether it applies."""

from ..config import company
from ..countries import EU

EXPORT_NOTE = "VAT exempt: export of goods or services outside the EU (Article 146, Directive 2006/112/EC)."


def payment_terms_note(invoice) -> str:
    return f"Payment due within {company().payment_terms_days} days, by {invoice.due_on.isoformat()}."


def notes_for(invoice):
    """The notes for an invoice, in printing order."""
    notes = [payment_terms_note(invoice)]
    if invoice.customer.country not in EU:
        notes.append(EXPORT_NOTE)
    return notes
