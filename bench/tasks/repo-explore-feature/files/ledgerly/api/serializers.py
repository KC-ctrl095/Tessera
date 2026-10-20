"""JSON shapes of the API (amounts as strings with two decimals)."""


def money(v):
    return f"{v:.2f}"


def customer_to_dict(c):
    return {"id": c.id, "name": c.name, "country": c.country, "kind": c.kind, "vat_id": c.vat_id, "email": c.email}


def line_to_dict(l):
    return {
        "sku": l.product.sku,
        "name": l.product.name,
        "quantity": l.quantity,
        "unit_price": money(l.unit_price),
        "vat_rate": str(l.vat_rate),
        "net": money(l.net),
        "vat": money(l.vat),
        "gross": money(l.gross),
    }


def invoice_to_dict(inv):
    return {
        "number": inv.number,
        "customer": customer_to_dict(inv.customer),
        "issued_on": inv.issued_on.isoformat(),
        "due_on": inv.due_on.isoformat(),
        "currency": inv.currency,
        "lines": [line_to_dict(l) for l in inv.lines],
        "net_total": money(inv.net_total),
        "vat_total": money(inv.vat_total),
        "gross_total": money(inv.gross_total),
        "notes": list(inv.notes),
    }
