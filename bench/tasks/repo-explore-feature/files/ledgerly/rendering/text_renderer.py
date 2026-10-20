"""Plain-text invoices (the PDF service renders the same text)."""

from pathlib import Path

from ..config import company
from ..countries import name as country_name
from ..money import fmt

TEMPLATE = Path(__file__).parent / "templates" / "invoice.txt"


def _line(l, currency):
    return f"{l.quantity:>4} x {l.product.name:<30} {fmt(l.unit_price, currency):>14}  VAT {l.vat_rate}%  {fmt(l.net, currency):>14}"


def render_invoice_text(invoice) -> str:
    seller = company()
    c = invoice.customer
    return TEMPLATE.read_text(encoding="utf-8").format(
        seller_name=seller.name,
        seller_address=seller.address,
        seller_vat_id=seller.vat_id,
        number=invoice.number,
        issued_on=invoice.issued_on.isoformat(),
        due_on=invoice.due_on.isoformat(),
        customer_name=c.name,
        customer_country=country_name(c.country),
        lines="\n".join(_line(l, invoice.currency) for l in invoice.lines),
        net_total=fmt(invoice.net_total, invoice.currency),
        vat_total=fmt(invoice.vat_total, invoice.currency),
        gross_total=fmt(invoice.gross_total, invoice.currency),
        notes="\n".join(invoice.notes),
    )
