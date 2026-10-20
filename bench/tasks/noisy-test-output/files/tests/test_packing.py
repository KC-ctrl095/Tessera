import unittest

from dockyard.fixtures import make_catalog, make_orders
from dockyard.packing import BOXES, DoesNotFit, box_cm3, fits, pack, smallest_box_for

from .support import small_catalog

S, M, L, XL = BOXES


class PackingTest(unittest.TestCase):
    def setUp(self):
        self.cat = small_catalog()

    def items(self, *skus):
        return [(s, self.cat.get(s)) for s in skus]

    def test_fits_in_any_orientation(self):
        self.assertTrue(fits(S, (90, 240, 170)))

    def test_too_long_does_not_fit(self):
        self.assertFalse(fits(S, (260, 10, 10)))

    def test_smallest_box(self):
        self.assertEqual(smallest_box_for((120, 80, 30), 180).code, "S")

    def test_heavy_small_item_needs_a_bigger_box(self):
        self.assertEqual(smallest_box_for((120, 80, 30), 1950).code, "M")

    def test_too_big_for_any_box(self):
        with self.assertRaises(DoesNotFit):
            smallest_box_for((700, 100, 100), 100)

    def test_small_items_share_a_box(self):
        parcels = pack(self.items("AB-104", "AB-104", "AB-104", "AB-104"))
        self.assertEqual([(p.box.code, len(p.skus), p.weight_g) for p in parcels], [("S", 4, 840)])

    def test_overflow_opens_a_second_box(self):
        parcels = pack(self.items("MN-402", "MN-402"))
        self.assertEqual([p.box.code for p in parcels], ["L", "L"])

    def test_largest_items_go_first(self):
        parcels = pack(self.items("AB-104", "MN-402"))
        self.assertEqual([(p.box.code, p.skus) for p in parcels], [("L", ["MN-402", "AB-104"])])

    def test_weight_includes_the_box(self):
        (p,) = pack(self.items("CD-211"))
        self.assertEqual((p.box.code, p.weight_g), ("M", 1360))

    def test_box_volume(self):
        self.assertEqual(box_cm3(S), 4500)


class SamplePackingTest(unittest.TestCase):
    def test_sample_orders_pack_within_limits(self):
        cat = make_catalog(8)
        for o in make_orders(cat, 150, seed=8):
            items = [(cat.get(l.sku).sku, cat.get(l.sku)) for l in o.lines for _ in range(l.qty)]
            parcels = pack(items)
            self.assertEqual(sum(len(p.skus) for p in parcels), len(items))
            for p in parcels:
                self.assertLessEqual(p.weight_g, p.box.max_weight_g)


if __name__ == "__main__":
    unittest.main()
