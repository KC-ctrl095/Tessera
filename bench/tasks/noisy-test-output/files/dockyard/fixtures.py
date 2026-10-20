"""Sample warehouses, catalogue, stock and orders for tests and demos (deterministic)."""

import logging
import random
from dataclasses import dataclass
from datetime import datetime, timedelta
from decimal import Decimal

from .catalog import Catalog
from .events import EventBus
from .inventory import Inventory
from .models import Order, OrderLine, Warehouse
from .store import Store

log = logging.getLogger(__name__)

WAREHOUSES = (
    Warehouse("AMS-1", "NL", "1043", 15),
    Warehouse("RTM-2", "NL", "3089", 15),
    Warehouse("EIN-1", "NL", "5651", 14),
    Warehouse("ANR-1", "BE", "2030", 16),
    Warehouse("CGN-1", "DE", "50829", 15),
)
DESTINATIONS = (
    ("NL", "1011"), ("NL", "1017"), ("NL", "2511"), ("NL", "3011"), ("NL", "3512"), ("NL", "5611"),
    ("NL", "6511"), ("NL", "7511"), ("NL", "9711"), ("BE", "1000"), ("BE", "2000"), ("BE", "4000"),
    ("BE", "9000"), ("DE", "10115"), ("DE", "20095"), ("DE", "40213"), ("DE", "50667"), ("DE", "60311"), ("DE", "80331"),
)
FAMILIES = (
    ("AB", "Cable organiser"), ("CD", "Desk lamp"), ("EF", "Notebook"), ("GH", "Water bottle"),
    ("KL", "Phone stand"), ("MN", "Storage crate"),
)


@dataclass
class Context:
    catalog: Catalog
    inventory: Inventory
    warehouses: dict
    bus: EventBus
    store: Store


def make_catalog(seed=7, per_family=10):
    rng = random.Random(seed)
    cat = Catalog()
    for prefix, name in FAMILIES:
        for i in range(per_family):
            number = 100 + i * 7 + rng.randint(0, 6)
            big = prefix == "MN"
            dims = (rng.randint(150, 420), rng.randint(100, 300), rng.randint(60, 220)) if big else (rng.randint(40, 220), rng.randint(30, 160), rng.randint(10, 90))
            cat.add(
                f"{prefix}-{number}",
                f"{name} {number}",
                Decimal(rng.randint(299, 8999)) / 100,
                rng.randint(900, 4200) if big else rng.randint(40, 1400),
                dims,
                Decimal("0.09") if prefix == "EF" else None,
            )
    return cat


def make_context(seed=7, stock_scale=1):
    rng = random.Random(seed * 31 + 1)
    catalog = make_catalog(seed)
    inv = Inventory()
    for w in WAREHOUSES:
        for sku in catalog.skus():
            if rng.random() < 0.7:
                inv.receive(w.code, sku, rng.randint(0, 40) * stock_scale)
    ctx = Context(catalog, inv, {w.code: w for w in WAREHOUSES}, EventBus(), Store(f"shipments-{seed}"))
    log.debug("context %d ready: %d SKUs, %d warehouses", seed, len(catalog), len(WAREHOUSES))
    return ctx


def make_orders(catalog, n, seed=1, start=datetime(2026, 3, 2, 7, 0), express_share=0.2):
    """n orders spread over the working hours of the days from `start` on (March 2026: no holidays)."""
    rng = random.Random(seed)
    skus = catalog.skus()
    orders = []
    t = start
    for i in range(n):
        t += timedelta(minutes=rng.randint(1, 25))
        if t.hour >= 21:
            t = t.replace(hour=7) + timedelta(days=1)
        country, postcode = rng.choice(DESTINATIONS)
        lines = [OrderLine(sku, rng.randint(1, 3)) for sku in rng.sample(skus, rng.randint(1, 4))]
        service = "express" if rng.random() < express_share else "standard"
        orders.append(Order(f"ORD-{seed:02d}{i:05d}", t, country, postcode, lines, service))
    return orders
