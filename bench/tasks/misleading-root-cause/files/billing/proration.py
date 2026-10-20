"""Plan changes in the middle of a billing period.

Days are Lisbon calendar days. A change made on a day counts that whole day at
the new price: the customer gets a credit for the old plan's remaining days and
pays the new plan's price for the same days.
"""

from core.money import share
from utils.tz import local_date


def days_between(start, end) -> int:
    """Lisbon days from the day of `start` up to (not including) the day of `end`."""
    return (local_date(end) - local_date(start)).days


def prorate(old_price_cents: int, new_price_cents: int, changed_at, period_start, period_end) -> dict:
    """What a plan change costs. All instants are UTC; the period is [start, end)."""
    total = days_between(period_start, period_end)
    left = days_between(changed_at, period_end)
    if not 0 <= left <= total:
        raise ValueError("the change isn't inside the billing period")
    credit = share(old_price_cents, left, total)
    charge = share(new_price_cents, left, total)
    return {"days_total": total, "days_left": left, "credit": credit, "charge": charge, "due": charge - credit}
