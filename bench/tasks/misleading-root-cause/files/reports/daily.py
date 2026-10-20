"""Revenue per Lisbon day, for the dashboard's bar chart."""

from utils.periods import day_bounds, days_of_month

from .aggregate import total_cents


def daily_totals(store, year: int, month: int) -> list:
    """[(day, cents)] for every day of the month, zero days included."""
    out = []
    for day in days_of_month(year, month):
        start, end = day_bounds(day)
        out.append((day, total_cents(store.payments_between(start, end))))
    return out


def best_day(store, year: int, month: int):
    days = daily_totals(store, year, month)
    return max(days, key=lambda d: (d[1], -d[0].toordinal()))
