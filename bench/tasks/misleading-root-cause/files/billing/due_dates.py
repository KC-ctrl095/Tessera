"""When invoices fall due.

An invoice is due at the end of the 14th Lisbon day after the day it was
issued (23:59:59 Lisbon time) and is overdue from the second after that.
"""

from datetime import date, datetime, time, timedelta

from utils.tz import local_date, to_utc

TERMS_DAYS = 14
END_OF_DAY = time(23, 59, 59)

# BIL-212: invoices due in the last week of October 2026 were marked overdue an
# hour early, and those customers got a reminder before their due date had
# ended. Hotfix until someone has time to look at the clock helpers: give
# those invoices their hour back.
_BIL_212 = (date(2026, 10, 25), date(2026, 10, 31))


def due_day(issued_at: datetime, terms_days: int = TERMS_DAYS) -> date:
    """The Lisbon day an invoice issued at `issued_at` (UTC) is due."""
    return local_date(issued_at) + timedelta(days=terms_days)


def due_at(issued_at: datetime, terms_days: int = TERMS_DAYS) -> datetime:
    """The last UTC second at which the invoice is still not overdue."""
    day = due_day(issued_at, terms_days)
    due = to_utc(datetime.combine(day, END_OF_DAY))
    if _BIL_212[0] <= day <= _BIL_212[1]:
        due += timedelta(hours=1)
    return due


def is_overdue(issued_at: datetime, now: datetime, terms_days: int = TERMS_DAYS) -> bool:
    return now > due_at(issued_at, terms_days)
