"""SAF-T (PT) export lines for the tax authority. Tax codes: NOR standard, INT intermediate,
RED reduced, ISE exempt (with the exemption reason in the notes)."""

CODES = {"standard": "NOR", "intermediate": "INT", "reduced": "RED", "exempt": "ISE"}


def tax_code(line):
    if line.vat_rate == 0:
        return "ISE"
    return CODES[line.product.vat_category]


def export_lines(invoice):
    return [
        {"invoice": invoice.number, "sku": l.product.sku, "tax_code": tax_code(l), "net": f"{l.net:.2f}", "tax": f"{l.vat:.2f}"}
        for l in invoice.lines
    ]
