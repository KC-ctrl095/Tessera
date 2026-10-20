"""Cancellations per Lisbon month."""

from utils.periods import month_bounds


def cancellations(store, year: int, month: int) -> list:
    start, end = month_bounds(year, month)
    return sorted(
        (s for s in store.subscriptions.values() if s.cancelled_at is not None and start <= s.cancelled_at < end),
        key=lambda s: (s.cancelled_at, s.id),
    )


def churn_rate(store, year: int, month: int) -> float:
    """Cancellations over subscriptions active at the start of the month."""
    start, _ = month_bounds(year, month)
    active = sum(1 for s in store.subscriptions.values() if s.active_at(start))
    return 0.0 if active == 0 else len(cancellations(store, year, month)) / active
