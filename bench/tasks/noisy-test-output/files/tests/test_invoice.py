import unittest
from decimal import Decimal

from dockyard.fixtures import make_catalog, make_orders
from dockyard.invoice import build_invoice

from .support import order, small_catalog


class InvoiceTest(unittest.TestCase):
    def setUp(self):
        self.cat = small_catalog()

    def test_lines_and_totals(self):
        inv = build_invoice(order(("AB-104", 2), ("EF-120", 3)), self.cat)
        self.assertEqual([(l.sku, l.net, l.vat) for l in inv.lines], [("AB-104", Decimal("25.00"), Decimal("5.25")), ("EF-120", Decimal("14.97"), Decimal("1.35"))])
        self.assertEqual((inv.net, inv.vat, inv.gross), (Decimal("39.97"), Decimal("6.60"), Decimal("46.57")))

    def test_vat_rounds_half_up(self):
        inv = build_invoice(order(("AB-104", 1)), self.cat)
        self.assertEqual(inv.lines[0].vat, Decimal("2.63"))

    def test_shipping_is_a_line_with_vat(self):
        inv = build_invoice(order(("GH-330", 1)), self.cat, shipping=Decimal("5.25"))
        last = inv.lines[-1]
        self.assertEqual((last.sku, last.net, last.vat), ("SHIPPING", Decimal("5.25"), Decimal("1.10")))

    def test_free_shipping_has_no_line(self):
        inv = build_invoice(order(("GH-330", 1)), self.cat)
        self.assertEqual([l.sku for l in inv.lines], ["GH-330"])

    def test_lowercase_skus_are_priced(self):
        inv = build_invoice(order(("cd-211", 1)), self.cat)
        self.assertEqual(inv.lines[0].net, Decimal("39.95"))

    def test_skus_typed_by_hand_are_priced(self):
        # Partner feeds send SKUs the way people typed them.
        inv = build_invoice(order((" ab  104", 2), ("cd . 211", 1)), self.cat)
        self.assertEqual([l.sku for l in inv.lines], ["AB-104", "CD-211"])
        self.assertEqual(inv.net, Decimal("64.95"))

    def test_unknown_sku_raises(self):
        with self.assertRaises(KeyError):
            build_invoice(order(("ZZ-999", 1)), self.cat)

    def test_empty_order(self):
        inv = build_invoice(order(), self.cat)
        self.assertEqual((inv.net, inv.vat, inv.gross), (Decimal("0.00"), Decimal("0.00"), Decimal("0.00")))

    def test_descriptions_come_from_the_catalog(self):
        inv = build_invoice(order(("MN-402", 1)), self.cat)
        self.assertEqual(inv.lines[0].description, "Storage crate 40 l")

    def test_gross_is_net_plus_vat_for_sample_orders(self):
        cat = make_catalog(7)
        for o in make_orders(cat, 25, seed=5):
            inv = build_invoice(o, cat, Decimal("6.95"))
            self.assertEqual(inv.gross, inv.net + inv.vat)


if __name__ == "__main__":
    unittest.main()
