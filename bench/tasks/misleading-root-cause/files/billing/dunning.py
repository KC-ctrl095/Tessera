"""Payment reminders for unpaid invoices.

Reminders go out at 09:00 Lisbon time on the 1st, 7th and 14th day after the
due day. Nobody wants a reminder at night, so the time is local, not UTC.
"""

from datetime import datetime, time, timedelta

from utils.tz import to_utc

from .due_dates import TERMS_DAYS, due_day

SEND_AT = time(9, 0)
AFTER_DAYS = (1, 7, 14)


def reminder_times(issued_at: datetime, terms_days: int = TERMS_DAYS) -> list:
    """UTC instants of the reminders of an invoice issued at `issued_at`."""
    day = due_day(issued_at, terms_days)
    return [to_utc(datetime.combine(day + timedelta(days=n), SEND_AT)) for n in AFTER_DAYS]


def reminders_due(issued_at: datetime, now: datetime, sent: int) -> int:
    """How many reminders should have gone out by `now`, minus those already sent."""
    due = [t for t in reminder_times(issued_at) if t <= now]
    return max(0, len(due) - sent)
