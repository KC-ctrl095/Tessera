"""Summaries of transactions."""

from decimal import Decimal

from . import money


def summarize(transactions):
    """One row per category with its total and its number of transactions, sorted by category name."""
    totals, counts = {}, {}
    for t in transactions:
        totals[t.category] = totals.get(t.category, Decimal("0")) + t.amount
        counts[t.category] = counts.get(t.category, 0) + 1
    rows = [{"category": c, "total": money.round_cents(v), "count": counts[c]} for c, v in totals.items()]
    rows.sort(key=lambda r: r["category"])
    return rows


def monthly(transactions):
    """{"YYYY-MM": total} for every month with transactions, in date order."""
    out = {}
    for t in sorted(transactions, key=lambda t: t.day):
        key = t.day.strftime("%Y-%m")
        out[key] = out.get(key, Decimal("0")) + t.amount
    return {k: money.round_cents(v) for k, v in out.items()}


def render(rows):
    """The summary as a plain-text table."""
    width = max([len(r["category"]) for r in rows] + [8])
    lines = [f"{'category':<{width}}  {'total':>10}  {'count':>5}"]
    for r in rows:
        lines.append(f"{r['category']:<{width}}  {r['total']:>10}  {r['count']:>5}")
    return "\n".join(lines)
