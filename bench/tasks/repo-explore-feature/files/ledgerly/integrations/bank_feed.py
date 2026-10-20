"""Matches bank transactions to open invoices by reference."""

import re

REF = re.compile(r"FT\s?(\d{4})/(\d{5})")


def match(transactions, invoices):
    by_number = {inv.number: inv for inv in invoices}
    out = []
    for t in transactions:
        m = REF.search(t.get("reference", ""))
        number = f"FT {m.group(1)}/{m.group(2)}" if m else None
        out.append((t, by_number.get(number)))
    return out
