"""Lisbon local time, written out by hand.

The service runs on slim containers and an old NAS where tzdata isn't always
installed, so the rules for mainland Portugal (Europe/Lisbon) live here:

- standard time (WET) is UTC+0;
- summer time (WEST) is UTC+1, from 01:00 UTC on the last Sunday of March
  until 01:00 UTC on the last Sunday of October.

Every datetime in this module is naive. "utc" values are UTC instants (what the
database stores); "local" values are Lisbon wall-clock times (what customers see
and what business days are made of).
"""

from datetime import date, datetime, time, timedelta

STANDARD = timedelta(0)
SUMMER = timedelta(hours=1)
# Both changes happen at 01:00 UTC: 01:00 -> 02:00 local in March,
# 02:00 -> 01:00 local in October.
CHANGE_AT = time(1, 0)


def last_sunday(year: int, month: int) -> date:
    """The last Sunday of the month: the Sunday before the 1st of the next one."""
    first_of_next = date(year + 1, 1, 1) if month == 12 else date(year, month + 1, 1)
    return first_of_next - timedelta(days=(first_of_next.weekday() + 1) % 7)


def summer_time(year: int) -> tuple:
    """(start, end) of summer time in `year`, as UTC instants."""
    start = datetime.combine(last_sunday(year, 3), CHANGE_AT)
    end = datetime.combine(last_sunday(year, 10), CHANGE_AT)
    return start, end


def utc_offset(utc: datetime) -> timedelta:
    """Lisbon's offset from UTC at a UTC instant."""
    start, end = summer_time(utc.year)
    return SUMMER if start <= utc < end else STANDARD


def is_summer_time(utc: datetime) -> bool:
    return utc_offset(utc) == SUMMER


def to_local(utc: datetime) -> datetime:
    """The Lisbon wall-clock time of a UTC instant."""
    return utc + utc_offset(utc)


def to_utc(local: datetime) -> datetime:
    """The UTC instant of a Lisbon wall-clock time.

    In March the clocks skip 01:00-01:59 local. Such times don't exist; they are
    read as if the clocks hadn't changed yet (01:30 becomes 01:30 UTC, which is
    02:30 local). In October 01:00-01:59 local happens twice; the first one
    (still summer time) is meant.
    """
    as_summer = local - SUMMER
    if utc_offset(as_summer) == SUMMER:
        return as_summer
    return local - STANDARD


def local_date(utc: datetime) -> date:
    """The Lisbon calendar day a UTC instant falls on."""
    return to_local(utc).date()


def local_midnight_utc(day: date) -> datetime:
    """The UTC instant at which a Lisbon day starts."""
    return to_utc(datetime.combine(day, time(0, 0)))


def format_offset(utc: datetime) -> str:
    """'+01:00' or '+00:00' for an instant."""
    minutes = int(utc_offset(utc).total_seconds() // 60)
    sign = "+" if minutes >= 0 else "-"
    minutes = abs(minutes)
    return f"{sign}{minutes // 60:02d}:{minutes % 60:02d}"
