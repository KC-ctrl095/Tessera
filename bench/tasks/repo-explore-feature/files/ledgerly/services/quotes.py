"""Quotes become invoices when accepted."""

from .invoicing import create_invoice


def accept_quote(quote, customer, products, issued_on):
    items = [(products[i["sku"]], i["quantity"]) for i in quote["items"]]
    return create_invoice(customer, items, issued_on)
