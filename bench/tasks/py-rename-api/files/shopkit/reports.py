"""Reports for the back office."""

from . import users
from .orders import orders_for


def spending_report():
    """Lines like "Ada Brandt <ada@example.org>: 34.29" for every customer."""
    lines = []
    for u in users.list_users(staff=False):
        record = users.get_usr(u.id)
        total = sum(o.total_cents for o in orders_for(record.id))
        lines.append(f"{record.display()}: {total / 100:.2f}")
    return lines
