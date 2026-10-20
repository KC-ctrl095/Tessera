import unittest

from dockyard.fixtures import WAREHOUSES, make_context, make_orders
from dockyard.models import Warehouse
from dockyard.routing import NoStock, by_distance, route

from .support import order, small_context


class RoutingTest(unittest.TestCase):
    def test_nearest_warehouse_ships_everything(self):
        ctx = small_context()
        plan = route(order(("AB-104", 1), ("CD-211", 2), postcode="3011"), WAREHOUSES, ctx.inventory)
        self.assertEqual(list(plan), ["RTM-2"])

    def test_warehouses_by_distance_from_amsterdam(self):
        ranked = by_distance(order(("AB-104", 1)), WAREHOUSES)
        self.assertEqual([w.code for w in ranked], ["AMS-1", "RTM-2", "EIN-1", "ANR-1", "CGN-1"])

    def test_skips_a_warehouse_without_stock(self):
        ctx = small_context(stock={("RTM-2", "AB-104"): 5, ("EIN-1", "AB-104"): 5})
        plan = route(order(("AB-104", 2)), WAREHOUSES, ctx.inventory)
        self.assertEqual(list(plan), ["RTM-2"])

    def test_splits_when_nobody_has_everything(self):
        ctx = small_context(stock={("RTM-2", "AB-104"): 5, ("CGN-1", "CD-211"): 5})
        plan = route(order(("AB-104", 1), ("CD-211", 1)), WAREHOUSES, ctx.inventory)
        self.assertEqual({k: [l.sku for l in v] for k, v in plan.items()}, {"RTM-2": ["AB-104"], "CGN-1": ["CD-211"]})

    def test_no_stock_anywhere(self):
        ctx = small_context(stock={})
        with self.assertRaises(NoStock):
            route(order(("AB-104", 1)), WAREHOUSES, ctx.inventory)

    def test_belgian_order_goes_to_antwerp(self):
        ctx = small_context()
        self.assertEqual(list(route(order(("GH-330", 1), country="BE", postcode="2000"), WAREHOUSES, ctx.inventory)), ["ANR-1"])

    def test_german_order_goes_to_cologne(self):
        ctx = small_context()
        self.assertEqual(list(route(order(("GH-330", 1), country="DE", postcode="80331"), WAREHOUSES, ctx.inventory)), ["CGN-1"])

    def test_ties_go_to_the_lower_code(self):
        twins = [Warehouse("AMS-9", "NL", "1050"), Warehouse("AMS-3", "NL", "1060")]
        self.assertEqual([w.code for w in by_distance(order(("AB-104", 1)), twins)], ["AMS-3", "AMS-9"])

    def test_not_enough_units_counts_as_no_stock(self):
        ctx = small_context(stock={("AMS-1", "AB-104"): 1})
        with self.assertRaises(NoStock):
            route(order(("AB-104", 2)), WAREHOUSES, ctx.inventory)


class SampleRoutingTest(unittest.TestCase):
    def test_sample_orders_go_to_a_warehouse_with_stock(self):
        ctx = make_context(5, stock_scale=4)
        routed = 0
        for o in make_orders(ctx.catalog, 300, seed=5):
            try:
                plan = route(o, WAREHOUSES, ctx.inventory)
            except NoStock:
                continue
            routed += 1
            for code, lines in plan.items():
                for line in lines:
                    self.assertGreaterEqual(ctx.inventory.available(code, line.sku), line.qty)
        self.assertGreater(routed, 250)


if __name__ == "__main__":
    unittest.main()
