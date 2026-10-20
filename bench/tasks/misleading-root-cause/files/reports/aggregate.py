"""Sums and groupings over lists of payments."""

from collections import Counter, defaultdict


def total_cents(payments) -> int:
    return sum(p.amount_cents for p in payments)


def by_plan(payments) -> dict:
    out = defaultdict(int)
    for p in payments:
        out[p.plan_code] += p.amount_cents
    return dict(sorted(out.items()))


def by_kind(payments) -> dict:
    out = defaultdict(int)
    for p in payments:
        out[p.kind] += p.amount_cents
    return dict(sorted(out.items()))


def count_by_kind(payments) -> dict:
    return dict(sorted(Counter(p.kind for p in payments).items()))


def paying_customers(payments) -> int:
    return len({p.customer_id for p in payments if p.amount_cents > 0})
