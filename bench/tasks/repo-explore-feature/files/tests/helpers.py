from datetime import date
from decimal import Decimal

from ledgerly import config
from ledgerly.models import Customer, Product
from ledgerly.services import numbering

LAPTOP = Product("LT-14", "Laptop stand", Decimal("49.90"))
BOOK = Product("BK-01", "Field guide", Decimal("20.00"), "reduced")
ISSUED = date(2026, 10, 1)


def fresh():
    config.reset()
    numbering.reset()


def customer(country, kind="business", vat_id=None, cid=1):
    return Customer(id=cid, name=f"Customer {country}", country=country, kind=kind, vat_id=vat_id, email="ap@example.org")
