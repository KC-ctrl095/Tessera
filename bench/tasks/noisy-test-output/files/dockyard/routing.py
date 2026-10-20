"""Which warehouse ships an order.

The nearest warehouse that has every line in stock ships the whole order. When
none has everything, the order is split: each line goes to the nearest
warehouse that has enough of it.
"""

import logging

from .geo import distance_between

log = logging.getLogger(__name__)


class NoStock(Exception):
    pass


def by_distance(order, warehouses):
    ranked = sorted(
        warehouses,
        key=lambda w: (distance_between(w.country, w.postcode, order.country, order.postcode), w.code),
    )
    log.debug("order %s: warehouses by distance %s", order.id, [w.code for w in ranked])
    return ranked


def route(order, warehouses, inventory):
    """{warehouse code: [order lines]} for the order."""
    ranked = by_distance(order, warehouses)
    for w in ranked:
        log.debug("order %s: trying %s for the whole order", order.id, w.code)
        if inventory.can_fulfil(w.code, order.lines):
            log.debug("order %s: %s ships everything", order.id, w.code)
            return {w.code: list(order.lines)}
    plan = {}
    for line in order.lines:
        for w in ranked:
            if inventory.available(w.code, line.sku) >= line.qty:
                plan.setdefault(w.code, []).append(line)
                log.debug("order %s: line %s x %d from %s", order.id, line.sku, line.qty, w.code)
                break
        else:
            raise NoStock(f"order {order.id}: nobody has {line.qty} x {line.sku}")
    log.info("order %s split over %s", order.id, sorted(plan))
    return plan
