"""Invoices for shipped orders: one line per order line plus shipping, VAT per line."""

import logging
from dataclasses import dataclass, field
from decimal import ROUND_HALF_UP, Decimal
from typing import List

from .catalog import normalize_sku

log = logging.getLogger(__name__)

CENT = Decimal("0.01")
SHIPPING_VAT = Decimal("0.21")


def money(x):
    return x.quantize(CENT, rounding=ROUND_HALF_UP)


@dataclass
class InvoiceLine:
    sku: str
    description: str
    qty: int
    unit_price: Decimal
    net: Decimal
    vat: Decimal


@dataclass
class Invoice:
    order_id: str
    lines: List[InvoiceLine] = field(default_factory=list)

    @property
    def net(self):
        return sum((l.net for l in self.lines), Decimal("0.00"))

    @property
    def vat(self):
        return sum((l.vat for l in self.lines), Decimal("0.00"))

    @property
    def gross(self):
        return self.net + self.vat


def build_invoice(order, catalog, shipping=Decimal("0.00")):
    prices = catalog.price_table()
    inv = Invoice(order.id)
    for line in order.lines:
        unit, vat_rate, name = prices[normalize_sku(line.sku)]
        net = money(unit * line.qty)
        vat = money(net * vat_rate)
        inv.lines.append(InvoiceLine(normalize_sku(line.sku), name, line.qty, unit, net, vat))
        log.debug("invoice %s: %d x %s @ %s = %s + VAT %s", order.id, line.qty, name, unit, net, vat)
    if shipping:
        inv.lines.append(InvoiceLine("SHIPPING", "Shipping", 1, shipping, money(shipping), money(shipping * SHIPPING_VAT)))
    log.debug("invoice %s: net %s, VAT %s, total %s", order.id, inv.net, inv.vat, inv.gross)
    return inv
