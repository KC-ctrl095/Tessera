"""Whole days of orders through the pipeline (slow-ish; these are the CI smoke tests)."""

import unittest
from collections import Counter
from decimal import Decimal

from dockyard.fixtures import make_context, make_orders
from dockyard.pipeline import Fulfilment


class SimulationTest(unittest.TestCase):
    def run_day(self, n, seed, stock_scale=4):
        ctx = make_context(seed, stock_scale=stock_scale)
        orders = make_orders(ctx.catalog, n, seed=seed)
        f = Fulfilment(ctx)
        return ctx, orders, f, f.simulate_day(orders)

    def test_busy_monday(self):
        _, _, _, s = self.run_day(100, 11)
        self.assertEqual(s["orders"] + s["no_stock"], 100)
        self.assertGreater(s["orders"], 80)

    def test_reservations_match_planned_units(self):
        ctx, orders, f, _ = self.run_day(80, 12)
        planned = Counter()
        for plans in f.plans.values():
            for p in plans:
                for parcel in p.parcels:
                    for sku in parcel.skus:
                        planned[(p.warehouse, sku)] += 1
        for (wh, sku), n in planned.items():
            self.assertEqual(ctx.inventory.on_hand(wh, sku) - ctx.inventory.available(wh, sku), n)

    def test_promises_never_come_before_shipping(self):
        _, _, f, _ = self.run_day(90, 13)
        for plans in f.plans.values():
            for p in plans:
                self.assertLessEqual(p.ship_date, p.promised)
                self.assertLess(p.ship_date.weekday(), 5)

    def test_split_orders_are_rare(self):
        _, _, _, s = self.run_day(90, 14)
        self.assertLess(s["split"], s["orders"] * 0.3)

    def test_revenue_is_the_sum_of_the_invoices(self):
        ctx = make_context(15, stock_scale=4)
        orders = make_orders(ctx.catalog, 60, seed=15)
        total = Decimal("0.00")
        f = Fulfilment(ctx)
        for o in orders:
            total += f.process(o)[1].gross
        ctx2 = make_context(15, stock_scale=4)
        s = Fulfilment(ctx2).simulate_day(make_orders(ctx2.catalog, 60, seed=15))
        self.assertEqual(s["revenue"], total)

    def test_every_order_is_accounted_for(self):
        ctx, orders, f, s = self.run_day(90, 16)
        backordered = ctx.bus.count("order.backordered")
        self.assertEqual(len(f.plans) + backordered, len(orders))
        self.assertEqual(backordered, s["no_stock"])


if __name__ == "__main__":
    unittest.main()
