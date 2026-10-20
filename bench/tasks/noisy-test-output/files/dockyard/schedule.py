"""Shipping days and delivery promises.

A warehouse ships on business days: Monday to Friday, except its country's
public holidays (data/holidays.json). An order placed before the warehouse's
cut-off hour on a business day ships that day; otherwise it ships on the next
business day. Transit time counts business days of the same calendar.
"""

import json
import logging
from datetime import date, timedelta
from functools import lru_cache
from pathlib import Path

log = logging.getLogger(__name__)

HOLIDAYS_FILE = Path(__file__).with_name("data") / "holidays.json"


@lru_cache(maxsize=None)
def load_holidays(country):
    """The public holidays of a country (ISO code) as a frozenset of dates."""
    with open(HOLIDAYS_FILE, encoding="utf-8") as f:
        data = json.load(f)
    entry = data["countries"].get(country.upper(), {})
    days = frozenset(date.fromisoformat(d) for d in entry.get("days", []))
    log.debug("loaded %d public holidays for %s", len(days), country)
    return days


def is_business_day(day, country):
    return day.weekday() < 5 and day not in load_holidays(country)


def next_business_day(day, country):
    d = day + timedelta(days=1)
    while not is_business_day(d, country):
        log.debug("%s is not a business day in %s", d.isoformat(), country)
        d += timedelta(days=1)
    return d


def add_business_days(day, n, country):
    d = day
    for _ in range(n):
        d = next_business_day(d, country)
    return d


def ship_date(placed_at, warehouse):
    """The day an order placed at `placed_at` (local time of the warehouse) leaves it."""
    day = placed_at.date()
    if is_business_day(day, warehouse.country) and placed_at.hour < warehouse.cutoff_hour:
        log.debug("placed %s before the %d:00 cut-off of %s: ships the same day", placed_at.isoformat(), warehouse.cutoff_hour, warehouse.code)
        return day
    d = next_business_day(day, warehouse.country)
    log.debug("placed %s: %s ships it on %s", placed_at.isoformat(), warehouse.code, d.isoformat())
    return d


def promised_date(placed_at, warehouse, transit):
    shipped = ship_date(placed_at, warehouse)
    due = add_business_days(shipped, transit, warehouse.country)
    log.debug("promise: ships %s + %d business day(s) -> %s", shipped.isoformat(), transit, due.isoformat())
    return shipped, due
