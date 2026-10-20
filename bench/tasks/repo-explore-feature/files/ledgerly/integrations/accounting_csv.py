"""CSV export for the accountant."""

import csv
import io


def to_csv(invoices):
    buf = io.StringIO()
    w = csv.writer(buf)
    w.writerow(["number", "issued_on", "customer", "country", "net", "vat", "gross"])
    for inv in invoices:
        w.writerow([inv.number, inv.issued_on.isoformat(), inv.customer.name, inv.customer.country,
                    f"{inv.net_total:.2f}", f"{inv.vat_total:.2f}", f"{inv.gross_total:.2f}"])
    return buf.getvalue()
