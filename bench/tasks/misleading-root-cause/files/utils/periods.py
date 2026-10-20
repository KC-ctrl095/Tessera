"""Business periods (Lisbon days, weeks and months) as UTC windows.

A window is a (start, end) pair of naive UTC datetimes; start is included and
end is not, so consecutive windows never overlap or leave gaps.
"""

from datetime import date, timedelta

from .dates import days_in_month, first_of_next_month
from .tz import local_midnight_utc


def day_bounds(day: date) -> tuple:
    """The UTC window of one Lisbon day (23, 24 or 25 hours long)."""
    return local_midnight_utc(day), local_midnight_utc(day + timedelta(days=1))


def day_length(day: date) -> timedelta:
    start, end = day_bounds(day)
    return end - start


def week_bounds(day: date) -> tuple:
    """The UTC window of the Lisbon week (Monday to Sunday) holding `day`."""
    monday = day - timedelta(days=day.weekday())
    return local_midnight_utc(monday), local_midnight_utc(monday + timedelta(days=7))


def month_bounds(year: int, month: int) -> tuple:
    """The UTC window of a Lisbon calendar month."""
    return local_midnight_utc(date(year, month, 1)), local_midnight_utc(first_of_next_month(year, month))


def days_of_month(year: int, month: int) -> list:
    return [date(year, month, d) for d in range(1, days_in_month(year, month) + 1)]


def previous_month(year: int, month: int) -> tuple:
    return (year - 1, 12) if month == 1 else (year, month - 1)


def next_month(year: int, month: int) -> tuple:
    return (year + 1, 1) if month == 12 else (year, month + 1)
