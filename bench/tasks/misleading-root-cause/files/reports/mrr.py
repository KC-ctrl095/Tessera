"""Monthly recurring revenue: the plan prices of every subscription active at
the end of a Lisbon day."""

from datetime import timedelta

from utils.tz import local_midnight_utc


def mrr_cents(store, day) -> int:
    at = local_midnight_utc(day + timedelta(days=1)) - timedelta(seconds=1)
    return sum(store.plan(s.plan_code).price_cents for s in store.subscriptions.values() if s.active_at(at))


def active_count(store, day) -> int:
    at = local_midnight_utc(day + timedelta(days=1)) - timedelta(seconds=1)
    return sum(1 for s in store.subscriptions.values() if s.active_at(at))
