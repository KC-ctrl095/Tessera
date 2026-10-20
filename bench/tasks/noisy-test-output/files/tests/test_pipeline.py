import unittest
from datetime import date
from decimal import Decimal

from dockyard.fixtures import make_context, make_orders
from dockyard.pipeline import Fulfilment, UnknownSku

from .support import order, small_context


class PipelineTest(unittest.TestCase):
    def test_single_warehouse_order(self):
        ctx = small_context()
        plans, invoice = Fulfilment(ctx).process(order(("AB-104", 2), ("CD-211", 1)))
        (p,) = plans
        self.assertEqual((p.warehouse, p.carrier, p.shipping, p.ship_date, p.promised, p.split), ("AMS-1", "DPD", Decimal("7.10"), date(2026, 3, 3), date(2026, 3, 5), False))

    def test_split_order(self):
        ctx = small_context(stock={("RTM-2", "AB-104"): 5, ("CGN-1", "CD-211"): 5})
        plans, _ = Fulfilment(ctx).process(order(("AB-104", 1), ("CD-211", 1)))
        self.assertEqual([(p.warehouse, p.split) for p in plans], [("CGN-1", True), ("RTM-2", True)])

    def test_unknown_sku(self):
        with self.assertRaises(UnknownSku):
            Fulfilment(small_context()).process(order(("ZZ-1", 1)))

    def test_quantity_must_be_positive(self):
        with self.assertRaises(ValueError):
            Fulfilment(small_context()).process(order(("AB-104", 0)))

    def test_units_are_reserved(self):
        ctx = small_context()
        Fulfilment(ctx).process(order(("AB-104", 2)))
        self.assertEqual(ctx.inventory.available("AMS-1", "AB-104"), 18)

    def test_express_ships_next_day(self):
        ctx = small_context()
        (p,), _ = Fulfilment(ctx).process(order(("GH-330", 1), service="express"))
        self.assertEqual((p.carrier, p.promised), ("PostNL", date(2026, 3, 4)))

    def test_express_abroad_needs_review(self):
        ctx = small_context(stock={("AMS-1", "GH-330"): 3})
        (p,), _ = Fulfilment(ctx).process(order(("GH-330", 1), country="BE", postcode="2000", service="express"))
        self.assertEqual((p.status, p.carrier), ("needs_review", None))

    def test_events_are_published(self):
        ctx = small_context()
        Fulfilment(ctx).process(order(("AB-104", 1)))
        self.assertEqual((ctx.bus.count("order.planned"), ctx.bus.count("order.invoiced")), (1, 1))

    def test_plans_are_stored(self):
        ctx = small_context()
        (p,), _ = Fulfilment(ctx).process(order(("AB-104", 1), oid="S-9"))
        self.assertIs(ctx.store.get("S-9@AMS-1"), p)

    def test_invoice_has_the_shipping_cost(self):
        ctx = small_context()
        (p,), inv = Fulfilment(ctx).process(order(("EF-120", 2)))
        self.assertEqual((inv.lines[-1].sku, inv.lines[-1].net), ("SHIPPING", p.shipping))

    def test_small_day(self):
        ctx = make_context(7)
        summary = Fulfilment(ctx).simulate_day(make_orders(ctx.catalog, 40, seed=3))
        self.assertEqual(summary["orders"] + summary["no_stock"], 40)
        self.assertEqual(summary["delivered"] + summary["failed_attempts"], summary["plans"])
        self.assertGreater(summary["revenue"], 0)

    def test_replay_plans_the_rest(self):
        ctx = make_context(7, stock_scale=5)
        orders = make_orders(ctx.catalog, 12, seed=4)
        f = Fulfilment(ctx)
        for o in orders[:5]:
            f.process(o)
        replanned = f.replay_day(orders)
        self.assertEqual(replanned, sum(1 for o in orders[5:] if o.id in f.plans))
        self.assertGreaterEqual(replanned, 4)

    def test_a_day_of_express_orders(self):
        ctx = make_context(21, stock_scale=4)
        f = Fulfilment(ctx)
        summary = f.simulate_day(make_orders(ctx.catalog, 150, seed=21, express_share=1.0))
        for plans in f.plans.values():
            for p in plans:
                if p.status != "needs_review":
                    self.assertLessEqual(p.promised.toordinal() - p.ship_date.toordinal(), 3)
        self.assertGreater(summary["orders"], 120)


if __name__ == "__main__":
    unittest.main()
