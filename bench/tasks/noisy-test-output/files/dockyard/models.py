"""Plain data types shared across the service."""

from dataclasses import dataclass, field
from datetime import date, datetime
from decimal import Decimal
from typing import List, Optional, Tuple


@dataclass(frozen=True)
class Warehouse:
    code: str  # e.g. "AMS-1"
    country: str  # ISO 3166 code, upper case: "NL", "BE", "DE"
    postcode: str
    cutoff_hour: int = 15  # orders placed before this hour ship the same business day


@dataclass
class CatalogEntry:
    sku: str
    name: str
    price: Decimal  # per unit, without VAT
    weight_g: int
    dims_mm: Tuple[int, int, int]
    vat_rate: Decimal = Decimal("0.21")


@dataclass
class OrderLine:
    sku: str
    qty: int


@dataclass
class Order:
    id: str
    placed_at: datetime
    country: str
    postcode: str
    lines: List[OrderLine]
    service: str = "standard"  # "standard" or "express"


@dataclass(frozen=True)
class Box:
    code: str
    inner_mm: Tuple[int, int, int]
    max_weight_g: int
    tare_g: int


@dataclass
class Parcel:
    box: Box
    skus: List[str] = field(default_factory=list)
    content_g: int = 0
    content_cm3: float = 0.0

    @property
    def weight_g(self):
        return self.box.tare_g + self.content_g


@dataclass
class Quote:
    carrier: str
    price: Decimal
    days: int


@dataclass
class ShipmentPlan:
    order_id: str
    warehouse: str
    parcels: List[Parcel]
    carrier: Optional[str]
    shipping: Decimal
    ship_date: Optional[date]
    promised: Optional[date]
    split: bool = False
    status: str = "planned"
