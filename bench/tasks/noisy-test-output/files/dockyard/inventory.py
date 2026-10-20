"""Stock levels per warehouse, and reservations against them."""

import logging
from collections import defaultdict

from .catalog import normalize_sku

log = logging.getLogger(__name__)

LOW_STOCK = 5  # a warning goes out when a reservation leaves fewer units than this


class OutOfStock(Exception):
    pass


class Inventory:
    def __init__(self):
        self._stock = defaultdict(int)  # (warehouse code, canonical sku) -> units on hand
        self._reserved = defaultdict(int)

    def receive(self, warehouse, sku, qty):
        key = (warehouse, normalize_sku(sku))
        self._stock[key] += qty
        log.debug("received %d x %s at %s (now %d)", qty, key[1], warehouse, self._stock[key])

    def available(self, warehouse, sku):
        key = (warehouse, normalize_sku(sku))
        free = self._stock[key] - self._reserved[key]
        log.debug("available %s at %s: %d (on hand %d, reserved %d)", key[1], warehouse, free, self._stock[key], self._reserved[key])
        return free

    def can_fulfil(self, warehouse, lines):
        for line in lines:
            if self.available(warehouse, line.sku) < line.qty:
                log.debug("%s can't fulfil %s x %d", warehouse, line.sku, line.qty)
                return False
        return True

    def reserve(self, warehouse, lines):
        """Reserve every line or none of them."""
        if not self.can_fulfil(warehouse, lines):
            raise OutOfStock(f"{warehouse} can't fulfil {[(l.sku, l.qty) for l in lines]}")
        for line in lines:
            key = (warehouse, normalize_sku(line.sku))
            self._reserved[key] += line.qty
            left = self._stock[key] - self._reserved[key]
            log.debug("reserved %d x %s at %s, %d left", line.qty, key[1], warehouse, left)
            if left < LOW_STOCK:
                log.warning("stock of %s at %s is low: %d left", key[1], warehouse, left)

    def release(self, warehouse, lines):
        for line in lines:
            key = (warehouse, normalize_sku(line.sku))
            self._reserved[key] = max(0, self._reserved[key] - line.qty)
            log.debug("released %d x %s at %s", line.qty, key[1], warehouse)

    def ship(self, warehouse, lines):
        """Reserved units leave the building."""
        for line in lines:
            key = (warehouse, normalize_sku(line.sku))
            self._reserved[key] -= line.qty
            self._stock[key] -= line.qty
            log.debug("shipped %d x %s from %s (on hand %d)", line.qty, key[1], warehouse, self._stock[key])

    def on_hand(self, warehouse, sku):
        return self._stock[(warehouse, normalize_sku(sku))]
