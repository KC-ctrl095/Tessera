import unittest
from decimal import Decimal

from dockyard import units


class UnitsTest(unittest.TestCase):
    def test_pounds_to_kilograms(self):
        self.assertEqual(units.convert_mass(Decimal(1), "lb", "kg"), Decimal("0.45359237"))

    def test_ounces_to_grams(self):
        self.assertEqual(units.convert_mass(Decimal(2), "oz", "g"), Decimal("56.699046250"))

    def test_old_helper_still_works_but_warns(self):
        with self.assertWarns(DeprecationWarning):
            self.assertEqual(units.lbs_to_kg(Decimal(2)), Decimal("0.90718474"))

    def test_mm_to_cm_warns(self):
        with self.assertWarns(DeprecationWarning):
            self.assertEqual(units.mm_to_cm(125), 12.5)

    def test_volume(self):
        self.assertEqual(units.volume_cm3((100, 200, 300)), 6000)

    def test_volumetric_weight_rounds_up(self):
        self.assertEqual(units.volumetric_weight_g((101, 101, 100)), 205)


if __name__ == "__main__":
    unittest.main()
