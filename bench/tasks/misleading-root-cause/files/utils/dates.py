"""Calendar arithmetic and the ISO formats used in files and the API."""

import calendar
from datetime import date, datetime

UTC_FORMAT = "%Y-%m-%dT%H:%M:%SZ"


def days_in_month(year: int, month: int) -> int:
    return calendar.monthrange(year, month)[1]


def first_of_next_month(year: int, month: int) -> date:
    return date(year + 1, 1, 1) if month == 12 else date(year, month + 1, 1)


def clamp_day(year: int, month: int, day: int) -> date:
    """`day` of the month, or the month's last day when the month is shorter."""
    return date(year, month, min(day, days_in_month(year, month)))


def add_months(year: int, month: int, n: int) -> tuple:
    index = year * 12 + (month - 1) + n
    return index // 12, index % 12 + 1


def parse_utc(text: str) -> datetime:
    """'2026-03-01T09:30:00Z' -> naive UTC datetime."""
    text = text.strip()
    if not text.endswith("Z"):
        raise ValueError(f"not a UTC timestamp: {text!r}")
    return datetime.strptime(text, UTC_FORMAT)


def format_utc(utc: datetime) -> str:
    return utc.strftime(UTC_FORMAT)


def parse_month(text: str) -> tuple:
    """'2026-10' -> (2026, 10)."""
    year, _, month = text.partition("-")
    y, m = int(year), int(month)
    if not 1 <= m <= 12:
        raise ValueError(f"no such month: {text!r}")
    return y, m
