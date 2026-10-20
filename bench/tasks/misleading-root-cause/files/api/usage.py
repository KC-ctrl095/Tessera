"""API usage quotas.

Partner integrations may make DAILY_QUOTA calls per Lisbon day. The counter
resets at Lisbon midnight, so on the days the clocks change a "day" lasts 23
or 25 hours.
"""

from collections import defaultdict
from datetime import datetime

from utils.periods import day_bounds
from utils.tz import local_date, to_local

DAILY_QUOTA = 500


def usage_window(now: datetime) -> tuple:
    """The UTC window of the Lisbon day `now` falls on."""
    return day_bounds(local_date(now))


class UsageMeter:
    def __init__(self, quota: int = DAILY_QUOTA):
        self.quota = quota
        self._calls = defaultdict(list)

    def record(self, customer_id: int, at: datetime) -> None:
        self._calls[customer_id].append(at)

    def used(self, customer_id: int, now: datetime) -> int:
        start, end = usage_window(now)
        return sum(1 for t in self._calls[customer_id] if start <= t < end)

    def remaining(self, customer_id: int, now: datetime) -> int:
        return max(0, self.quota - self.used(customer_id, now))

    def allow(self, customer_id: int, now: datetime) -> bool:
        """Count a call at `now` if the quota allows it."""
        if self.used(customer_id, now) >= self.quota:
            return False
        self.record(customer_id, now)
        return True

    def resets_at(self, now: datetime) -> datetime:
        return usage_window(now)[1]


def hourly_counts(calls, day) -> dict:
    """{local hour: calls} for one Lisbon day (hours as shown on the wall clock)."""
    start, end = day_bounds(day)
    out = defaultdict(int)
    for t in calls:
        if start <= t < end:
            out[to_local(t).hour] += 1
    return dict(sorted(out.items()))
