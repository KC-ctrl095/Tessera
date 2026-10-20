"""In-memory store behind the API (the real deployment swaps in the database layer)."""

from dataclasses import dataclass, field


@dataclass
class Store:
    customers: dict = field(default_factory=dict)
    products: dict = field(default_factory=dict)
    invoices: dict = field(default_factory=dict)

    def clear(self):
        self.customers.clear()
        self.products.clear()
        self.invoices.clear()


STORE = Store()
