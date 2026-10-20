"""The records the service works with. Instants are naive UTC datetimes."""

from dataclasses import dataclass
from datetime import datetime, time
from typing import Optional


@dataclass(frozen=True)
class Customer:
    id: int
    name: str
    email: str
    country: str  # ISO code, decides the VAT rate


@dataclass(frozen=True)
class Plan:
    code: str
    name: str
    price_cents: int
    bags_per_month: int


@dataclass(frozen=True)
class Subscription:
    id: int
    customer_id: int
    plan_code: str
    started_at: datetime
    renewal_day: int  # day of the month; short months use their last day
    renewal_time: time  # Lisbon wall-clock time of the monthly charge
    cancelled_at: Optional[datetime] = None

    def active_at(self, at: datetime) -> bool:
        return self.started_at <= at and (self.cancelled_at is None or at < self.cancelled_at)


@dataclass(frozen=True)
class Payment:
    id: int
    subscription_id: int
    customer_id: int
    plan_code: str
    amount_cents: int  # negative for refunds
    paid_at: datetime
    kind: str  # "renewal", "extra" or "refund"
