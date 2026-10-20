"""The monthly revenue report finance closes the books with.

A month is a Lisbon calendar month: from local midnight on the 1st to local
midnight on the 1st of the next month.
"""

from dataclasses import dataclass, field

from utils.periods import month_bounds

from . import aggregate
from .format import money_table


@dataclass
class MonthlyReport:
    year: int
    month: int
    total_cents: int
    payment_count: int
    refunds_cents: int
    paying_customers: int
    by_plan: dict = field(default_factory=dict)
    by_kind: dict = field(default_factory=dict)

    @property
    def label(self) -> str:
        return f"{self.year}-{self.month:02d}"


def monthly_report(store, year: int, month: int) -> MonthlyReport:
    start, end = month_bounds(year, month)
    payments = store.payments_between(start, end)
    kinds = aggregate.by_kind(payments)
    return MonthlyReport(
        year=year,
        month=month,
        total_cents=aggregate.total_cents(payments),
        payment_count=len(payments),
        refunds_cents=kinds.get("refund", 0),
        paying_customers=aggregate.paying_customers(payments),
        by_plan=aggregate.by_plan(payments),
        by_kind=kinds,
    )


def render(report: MonthlyReport) -> str:
    rows = [(plan, cents) for plan, cents in report.by_plan.items()]
    rows.append(("refunds", report.refunds_cents))
    return "\n".join(
        [
            f"Revenue {report.label} (Lisbon time)",
            money_table(rows, total=report.total_cents),
            f"{report.payment_count} payments from {report.paying_customers} customers",
        ]
    )
