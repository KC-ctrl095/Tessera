"""Reading bank exports: CSV with date, description, amount and category columns."""

import csv
import io
from dataclasses import dataclass
from datetime import date
from decimal import Decimal

from . import money


@dataclass(frozen=True)
class Transaction:
    day: date
    description: str
    amount: Decimal
    category: str


def read_csv(text):
    """Transactions from the text of a CSV export (a missing category is "uncategorised")."""
    out = []
    for row in csv.DictReader(io.StringIO(text)):
        out.append(
            Transaction(
                day=date.fromisoformat(row["date"].strip()),
                description=row["description"].strip(),
                amount=money.parse_amount(row["amount"]),
                category=(row.get("category") or "").strip() or "uncategorised",
            )
        )
    return out


def read_file(path):
    with open(path, encoding="utf-8-sig") as f:
        return read_csv(f.read())
