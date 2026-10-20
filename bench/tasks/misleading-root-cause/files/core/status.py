"""Subscription status as shown to support staff."""

from datetime import timedelta

GRACE = timedelta(days=3)


def status(sub, now) -> str:
    if sub.started_at > now:
        return "scheduled"
    if sub.cancelled_at is not None:
        if now < sub.cancelled_at:
            return "ending"
        return "cancelled"
    return "active"


def recently_cancelled(sub, now) -> bool:
    return sub.cancelled_at is not None and sub.cancelled_at <= now < sub.cancelled_at + GRACE
