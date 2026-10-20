"""Order fulfilment from end to end: route, reserve, pack, pick carriers, promise a date, invoice."""

import hashlib
import logging
from datetime import timedelta
from decimal import Decimal

from . import carriers, schedule
from .invoice import build_invoice
from .models import ShipmentPlan
from .packing import pack
from .routing import NoStock, route

log = logging.getLogger(__name__)


class UnknownSku(ValueError):
    pass


class Fulfilment:
    def __init__(self, ctx):
        self.ctx = ctx
        self.plans = {}

    def plan(self, order):
        ctx = self.ctx
        log.info("planning order %s: %d line(s) to %s %s, %s", order.id, len(order.lines), order.country, order.postcode, order.service)
        for line in order.lines:
            if line.sku not in ctx.catalog:
                raise UnknownSku(f"order {order.id}: unknown SKU {line.sku!r}")
            if line.qty <= 0:
                raise ValueError(f"order {order.id}: quantity must be positive, got {line.qty}")
        routing = route(order, list(ctx.warehouses.values()), ctx.inventory)
        plans = []
        for code, lines in sorted(routing.items()):
            wh = ctx.warehouses[code]
            ctx.inventory.reserve(code, lines)
            items = []
            for line in lines:
                entry = ctx.catalog.get(line.sku)
                items.extend((entry.sku, entry) for _ in range(line.qty))
            parcels = pack(items)
            shipping = Decimal("0.00")
            days = 0
            chosen = set()
            status = "planned"
            for i, parcel in enumerate(parcels, 1):
                q = carriers.choose(parcel, wh.country, order.country, order.service, f"{order.id}/{code}/{i}")
                if q is None:
                    status = "needs_review"
                    continue
                shipping += q.price
                days = max(days, q.days)
                chosen.add(q.carrier)
            shipped, due = schedule.promised_date(order.placed_at, wh, days)
            plan = ShipmentPlan(
                order_id=order.id,
                warehouse=code,
                parcels=parcels,
                carrier=", ".join(sorted(chosen)) or None,
                shipping=shipping,
                ship_date=shipped,
                promised=due,
                split=len(routing) > 1,
                status=status,
            )
            with ctx.store.cursor(f"plan {order.id}"):
                ctx.store.put(f"{order.id}@{code}", plan)
            ctx.bus.publish("order.planned", order=order.id, warehouse=code, parcels=len(parcels), carrier=plan.carrier,
                            shipping=shipping, ships=shipped, due=due, status=status)
            plans.append(plan)
        self.plans[order.id] = plans
        return plans

    def process(self, order):
        plans = self.plan(order)
        shipping = sum((p.shipping for p in plans), Decimal("0.00"))
        invoice = build_invoice(order, self.ctx.catalog, shipping)
        self.ctx.bus.publish("order.invoiced", order=order.id, total=invoice.gross)
        return plans, invoice

    def simulate_day(self, orders):
        """Process a day's orders and their first delivery attempts. Returns a summary dict."""
        summary = {"orders": 0, "plans": 0, "split": 0, "no_stock": 0, "delivered": 0, "failed_attempts": 0, "revenue": Decimal("0.00")}
        for order in orders:
            try:
                plans, invoice = self.process(order)
            except NoStock as e:
                log.warning("order %s not planned: %s", order.id, e)
                summary["no_stock"] += 1
                self.ctx.bus.publish("order.backordered", order=order.id)
                continue
            summary["orders"] += 1
            summary["plans"] += len(plans)
            summary["split"] += any(p.split for p in plans)
            summary["revenue"] += invoice.gross
            for plan in plans:
                self._deliver(plan)
                summary["delivered" if plan.status == "delivered" else "failed_attempts"] += 1
        log.info("day summary: %s", summary)
        return summary

    def _deliver(self, plan):
        ref = f"{plan.order_id}@{plan.warehouse}"
        roll = int(hashlib.sha1(ref.encode()).hexdigest(), 16) % 9
        if roll == 0:
            plan.status = "FAILED_ATTEMPT"
            log.info("shipment %s status=FAILED_ATTEMPT (nobody home), next attempt %s", ref, (plan.promised + timedelta(days=1)).isoformat())
        else:
            plan.status = "delivered"
            log.debug("shipment %s status=delivered on %s", ref, plan.promised.isoformat())
        self.ctx.bus.publish("shipment.status", shipment=ref, status=plan.status)

    def replay_day(self, orders):
        """Re-plan orders from a saved day (used by the nightly reconciliation)."""
        self.ctx.store.raw_cursor("pipeline.replay_day")
        replanned = 0
        for order in orders:
            if order.id in self.plans:
                continue
            try:
                self.plan(order)
                replanned += 1
            except NoStock:
                log.warning("replay: order %s still has no stock", order.id)
        log.info("replayed %d order(s)", replanned)
        return replanned
