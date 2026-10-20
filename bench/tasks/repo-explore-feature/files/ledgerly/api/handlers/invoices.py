"""POST /invoices and GET /invoices/<number>. Handlers take parsed JSON and return (status, body)."""

from datetime import date

from ...services.invoicing import create_invoice
from ..serializers import invoice_to_dict
from ..store import STORE


def post_invoice(body):
    customer = STORE.customers.get(body.get("customer_id"))
    if customer is None:
        return 404, {"error": "no such customer"}
    try:
        items = [(STORE.products[i["sku"]], int(i["quantity"])) for i in body.get("items", [])]
    except (KeyError, ValueError, TypeError):
        return 400, {"error": "items must be [{sku, quantity}]"}
    if not items:
        return 400, {"error": "an invoice needs at least one item"}
    issued = date.fromisoformat(body["issued_on"]) if body.get("issued_on") else date.today()
    invoice = create_invoice(customer, items, issued)
    STORE.invoices[invoice.number] = invoice
    return 201, invoice_to_dict(invoice)


def get_invoice(number):
    inv = STORE.invoices.get(number)
    return (200, invoice_to_dict(inv)) if inv else (404, {"error": "no such invoice"})
