import unittest

from dockyard.inventory import Inventory, OutOfStock
from dockyard.models import OrderLine


class InventoryTest(unittest.TestCase):
    def setUp(self):
        self.inv = Inventory()
        self.inv.receive("AMS-1", "AB-104", 10)
        self.inv.receive("AMS-1", "CD-211", 3)

    def test_receive_adds_up(self):
        self.inv.receive("AMS-1", "AB-104", 5)
        self.assertEqual(self.inv.available("AMS-1", "AB-104"), 15)

    def test_unknown_stock_is_zero(self):
        self.assertEqual(self.inv.available("RTM-2", "AB-104"), 0)

    def test_reserve_takes_units(self):
        self.inv.reserve("AMS-1", [OrderLine("AB-104", 4)])
        self.assertEqual(self.inv.available("AMS-1", "AB-104"), 6)
        self.assertEqual(self.inv.on_hand("AMS-1", "AB-104"), 10)

    def test_reserve_is_all_or_nothing(self):
        with self.assertRaises(OutOfStock):
            self.inv.reserve("AMS-1", [OrderLine("AB-104", 2), OrderLine("CD-211", 4)])
        self.assertEqual(self.inv.available("AMS-1", "AB-104"), 10)

    def test_release_gives_units_back(self):
        lines = [OrderLine("AB-104", 4)]
        self.inv.reserve("AMS-1", lines)
        self.inv.release("AMS-1", lines)
        self.assertEqual(self.inv.available("AMS-1", "AB-104"), 10)

    def test_release_never_goes_below_zero(self):
        self.inv.release("AMS-1", [OrderLine("AB-104", 4)])
        self.assertEqual(self.inv.available("AMS-1", "AB-104"), 10)

    def test_ship_removes_stock(self):
        lines = [OrderLine("CD-211", 2)]
        self.inv.reserve("AMS-1", lines)
        self.inv.ship("AMS-1", lines)
        self.assertEqual((self.inv.on_hand("AMS-1", "CD-211"), self.inv.available("AMS-1", "CD-211")), (1, 1))

    def test_low_stock_warning(self):
        with self.assertLogs("dockyard.inventory", "WARNING") as cm:
            self.inv.reserve("AMS-1", [OrderLine("AB-104", 7)])
        self.assertIn("stock of AB-104 at AMS-1 is low: 3 left", cm.output[0])

    def test_skus_are_matched_in_any_case(self):
        self.assertEqual(self.inv.available("AMS-1", "ab-104"), 10)

    def test_can_fulfil(self):
        self.assertTrue(self.inv.can_fulfil("AMS-1", [OrderLine("AB-104", 10), OrderLine("CD-211", 3)]))
        self.assertFalse(self.inv.can_fulfil("AMS-1", [OrderLine("CD-211", 4)]))


if __name__ == "__main__":
    unittest.main()
