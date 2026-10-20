from dataclasses import dataclass, field
from datetime import date
from decimal import Decimal
from typing import List

from ..money import percent_of, to_money
from .customer import Customer
from .product import Product


@dataclass
class InvoiceLine:
    product: Product
    quantity: int
    unit_price: Decimal
    vat_rate: Decimal  # percent

    @property
    def net(self) -> Decimal:
        return to_money(self.unit_price * self.quantity)

    @property
    def vat(self) -> Decimal:
        return percent_of(self.net, self.vat_rate)

    @property
    def gross(self) -> Decimal:
        return self.net + self.vat


@dataclass
class Invoice:
    number: str
    customer: Customer
    issued_on: date
    due_on: date
    currency: str
    lines: List[InvoiceLine] = field(default_factory=list)
    notes: List[str] = field(default_factory=list)

    @property
    def net_total(self) -> Decimal:
        return sum((l.net for l in self.lines), Decimal("0.00"))

    @property
    def vat_total(self) -> Decimal:
        return sum((l.vat for l in self.lines), Decimal("0.00"))

    @property
    def gross_total(self) -> Decimal:
        return self.net_total + self.vat_total
