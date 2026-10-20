import unittest
from decimal import Decimal

from dockyard import rates
from dockyard.models import Parcel
from dockyard.packing import BOXES

S, M, L, XL = BOXES


class BandTest(unittest.TestCase):
    def test_inside_a_band(self):
        self.assertEqual(rates.band_price("PostNL", "domestic", 1500), Decimal("6.95"))

    def test_band_limits_are_inclusive(self):
        self.assertEqual(rates.band_price("PostNL", "domestic", 2000), Decimal("6.95"))

    def test_just_over_a_limit(self):
        self.assertEqual(rates.band_price("PostNL", "domestic", 2001), Decimal("7.45"))

    def test_lightest_parcel(self):
        self.assertEqual(rates.band_price("PostNL", "domestic", 1), Decimal("4.10"))

    def test_too_heavy(self):
        with self.assertRaises(rates.NoRate):
            rates.band_price("DHL", "eu1", 25000)

    def test_dpd_first_band(self):
        self.assertEqual(rates.band_price("DPD", "domestic", 1500), Decimal("6.40"))

    def test_dhl_cross_border(self):
        self.assertEqual(rates.band_price("DHL", "eu1", 3000), Decimal("14.80"))


class ZoneAndWeightTest(unittest.TestCase):
    def test_zones(self):
        self.assertEqual((rates.zone("NL", "NL"), rates.zone("NL", "BE")), ("domestic", "eu1"))

    def test_transit_days(self):
        self.assertEqual((rates.transit_days("DPD", "eu1"), rates.transit_days("PostNL", "domestic")), (3, 1))

    def test_light_parcels_pay_volumetric_weight(self):
        self.assertEqual(rates.chargeable_weight_g(Parcel(S, ["X"], 180, 1.0)), 900)

    def test_heavy_parcels_pay_real_weight(self):
        self.assertEqual(rates.chargeable_weight_g(Parcel(S, ["X"], 1500, 1.0)), 1620)

    def test_volumetric_weight_of_each_box(self):
        self.assertEqual([rates.volumetric_weight_g(b.inner_mm) for b in BOXES], [900, 2625, 8750, 19200])


if __name__ == "__main__":
    unittest.main()
