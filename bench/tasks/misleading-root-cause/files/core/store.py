"""Loads data/*.csv into memory. The real service reads the same columns from
its database; the CSV export keeps the tests fast and self-contained."""

import bisect
from datetime import time
from pathlib import Path

from utils.csvio import optional, read_rows
from utils.dates import parse_utc

from .errors import NotFound
from .models import Customer, Payment, Plan, Subscription

DATA = Path(__file__).resolve().parent.parent / "data"


class Store:
    def __init__(self, customers, plans, subscriptions, payments):
        self.customers = {c.id: c for c in customers}
        self.plans = {p.code: p for p in plans}
        self.subscriptions = {s.id: s for s in subscriptions}
        self.payments = sorted(payments, key=lambda p: (p.paid_at, p.id))
        self._paid_at = [p.paid_at for p in self.payments]

    @classmethod
    def load(cls, folder=DATA):
        folder = Path(folder)
        customers = [
            Customer(int(r["id"]), r["name"], r["email"], r["country"]) for r in read_rows(folder / "customers.csv")
        ]
        plans = [
            Plan(r["code"], r["name"], int(r["price_cents"]), int(r["bags_per_month"]))
            for r in read_rows(folder / "plans.csv")
        ]
        subs = []
        for r in read_rows(folder / "subscriptions.csv"):
            hh, mm = r["renewal_time"].split(":")
            cancelled = optional(r["cancelled_at"])
            subs.append(
                Subscription(
                    id=int(r["id"]),
                    customer_id=int(r["customer_id"]),
                    plan_code=r["plan"],
                    started_at=parse_utc(r["started_at"]),
                    renewal_day=int(r["renewal_day"]),
                    renewal_time=time(int(hh), int(mm)),
                    cancelled_at=parse_utc(cancelled) if cancelled else None,
                )
            )
        payments = [
            Payment(
                id=int(r["id"]),
                subscription_id=int(r["subscription_id"]),
                customer_id=int(r["customer_id"]),
                plan_code=r["plan"],
                amount_cents=int(r["amount_cents"]),
                paid_at=parse_utc(r["paid_at"]),
                kind=r["kind"],
            )
            for r in read_rows(folder / "payments.csv")
        ]
        return cls(customers, plans, subs, payments)

    def customer(self, customer_id: int) -> Customer:
        try:
            return self.customers[customer_id]
        except KeyError:
            raise NotFound(f"no customer {customer_id}") from None

    def plan(self, code: str) -> Plan:
        try:
            return self.plans[code]
        except KeyError:
            raise NotFound(f"no plan {code!r}") from None

    def subscription(self, sub_id: int) -> Subscription:
        try:
            return self.subscriptions[sub_id]
        except KeyError:
            raise NotFound(f"no subscription {sub_id}") from None

    def payment(self, payment_id: int) -> Payment:
        for p in self.payments:
            if p.id == payment_id:
                return p
        raise NotFound(f"no payment {payment_id}")

    def payments_between(self, start, end) -> list:
        """Payments with start <= paid_at < end (UTC), oldest first."""
        lo = bisect.bisect_left(self._paid_at, start)
        hi = bisect.bisect_left(self._paid_at, end)
        return self.payments[lo:hi]

    def payments_of(self, customer_id: int) -> list:
        return [p for p in self.payments if p.customer_id == customer_id]
