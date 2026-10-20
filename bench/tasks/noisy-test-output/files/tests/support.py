"""Small, explicit fixtures for unit tests (the big random ones are in dockyard.fixtures)."""

from datetime import datetime
from decimal import Decimal

from dockyard.catalog import Catalog
from dockyard.events import EventBus
from dockyard.fixtures import WAREHOUSES, Context
from dockyard.inventory import Inventory
from dockyard.models import Order, OrderLine
from dockyard.store import Store

WH = {w.code: w for w in WAREHOUSES}


def small_catalog():
    cat = Catalog()
    cat.add("AB-104", "Cable organiser 104", Decimal("12.50"), 180, (120, 80, 30))
    cat.add("CD-211", "Desk lamp 211", Decimal("39.95"), 1150, (300, 160, 140))
    cat.add("EF-120", "Notebook A5", Decimal("4.99"), 240, (210, 148, 12), vat_rate=Decimal("0.09"))
    cat.add("GH-330", "Water bottle 0.75 l", Decimal("17.00"), 310, (260, 75, 75))
    cat.add("MN-402", "Storage crate 40 l", Decimal("24.90"), 2600, (400, 300, 200))
    return cat


def small_context(stock=None):
    """Every SKU of small_catalog() at every warehouse, 20 each, unless `stock` says otherwise."""
    cat = small_catalog()
    inv = Inventory()
    for w in WAREHOUSES:
        for sku in cat.skus():
            inv.receive(w.code, sku, 20 if stock is None else stock.get((w.code, sku), 0))
    return Context(cat, inv, dict(WH), EventBus(), Store("unit"))


def order(*lines, country="NL", postcode="1011", at=datetime(2026, 3, 3, 10, 0), service="standard", oid="T-1"):
    return Order(oid, at, country, postcode, [OrderLine(s, q) for s, q in lines], service)
