"""When subscriptions renew.

A subscription is charged every month on its renewal day (or the month's last
day when the month is shorter) at its renewal time, both in Lisbon local time:
a customer who signed up at 09:15 on the 31st is charged at 09:15 Lisbon time
on the 31st, the 30th of April, the 28th or 29th of February, and so on.
"""

from datetime import datetime, timedelta

from utils.dates import add_months, clamp_day
from utils.tz import to_local, to_utc


def renewal_local(sub, year: int, month: int) -> datetime:
    """The Lisbon wall-clock time of the subscription's charge in a month."""
    return datetime.combine(clamp_day(year, month, sub.renewal_day), sub.renewal_time)


def renewal_utc(sub, year: int, month: int) -> datetime:
    return to_utc(renewal_local(sub, year, month))


def next_renewal(sub, after: datetime):
    """The first charge strictly after `after` (UTC), or None once cancelled."""
    local = to_local(after)
    year, month = local.year, local.month
    for _ in range(3):
        at = renewal_utc(sub, year, month)
        if at > after and at > sub.started_at:
            if sub.cancelled_at is not None and at >= sub.cancelled_at:
                return None
            return at
        year, month = add_months(year, month, 1)
    raise AssertionError("unreachable: a renewal happens every month")


def renewals_between(sub, start: datetime, end: datetime) -> list:
    """Every charge with start <= at < end (UTC)."""
    out = []
    at = next_renewal(sub, start - timedelta(seconds=1))
    while at is not None and at < end:
        out.append(at)
        at = next_renewal(sub, at)
    return out
