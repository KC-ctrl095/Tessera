import unittest
from decimal import Decimal

from dockyard.catalog import Catalog, normalize_sku

from .support import small_catalog


class CatalogTest(unittest.TestCase):
    def setUp(self):
        self.cat = small_catalog()

    def test_get_by_canonical_sku(self):
        self.assertEqual(self.cat.get("CD-211").name, "Desk lamp 211")

    def test_lookup_ignores_case(self):
        self.assertEqual(self.cat.get("cd-211").sku, "CD-211")

    def test_lookup_trims_and_maps_underscores(self):
        self.assertEqual(self.cat.get("  gh_330 ").sku, "GH-330")

    def test_unknown_sku_is_none(self):
        self.assertIsNone(self.cat.get("ZZ-1"))

    def test_contains(self):
        self.assertIn("ab-104", self.cat)
        self.assertNotIn("ab-105", self.cat)

    def test_duplicates_are_rejected(self):
        with self.assertRaises(ValueError):
            self.cat.add("ab_104", "Again", Decimal("1.00"), 1, (1, 1, 1))

    def test_skus_are_sorted_and_canonical(self):
        self.assertEqual(self.cat.skus(), ["AB-104", "CD-211", "EF-120", "GH-330", "MN-402"])

    def test_price_table(self):
        price, vat, name = self.cat.price_table()["EF-120"]
        self.assertEqual((price, vat, name), (Decimal("4.99"), Decimal("0.09"), "Notebook A5"))

    def test_registration_stores_the_canonical_sku(self):
        cat = Catalog()
        e = cat.add(" kl_77 ", "Phone stand", Decimal("9.99"), 120, (90, 70, 20))
        self.assertEqual(e.sku, "KL-77")
        self.assertEqual(normalize_sku("kl-77"), "KL-77")


if __name__ == "__main__":
    unittest.main()
