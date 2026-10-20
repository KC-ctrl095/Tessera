"""Unpaid invoices by days overdue (0-30, 31-60, 61-90, 90+)."""

BUCKETS = [(30, "0-30"), (60, "31-60"), (90, "61-90")]


def bucket(days_overdue):
    for limit, label in BUCKETS:
        if days_overdue <= limit:
            return label
    return "90+"


def build(invoices, today, paid_numbers=()):
    out = {label: [] for _, label in BUCKETS}
    out["90+"] = []
    for inv in invoices:
        if inv.number in paid_numbers or inv.due_on >= today:
            continue
        out[bucket((today - inv.due_on).days)].append(inv.number)
    return out
