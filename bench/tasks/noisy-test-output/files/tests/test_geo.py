import unittest

from dockyard import geo


class GeoTest(unittest.TestCase):
    def test_amsterdam_to_rotterdam(self):
        self.assertAlmostEqual(geo.distance_between("NL", "1011", "NL", "3011"), 57.6, delta=1.5)

    def test_berlin_to_munich(self):
        self.assertAlmostEqual(geo.distance_between("DE", "10115", "DE", "80331"), 504, delta=10)

    def test_symmetric(self):
        a = geo.distance_between("BE", "9000", "DE", "50667")
        b = geo.distance_between("DE", "50667", "BE", "9000")
        self.assertAlmostEqual(a, b)

    def test_same_area_is_zero(self):
        self.assertEqual(geo.distance_between("NL", "1011", "NL", "1043"), 0)

    def test_letters_in_postcodes_are_ignored(self):
        self.assertEqual(geo.locate("nl", "1011 AB"), geo.AREAS[("NL", "10")])

    def test_unknown_area(self):
        with self.assertRaises(geo.UnknownArea):
            geo.locate("FR", "75001")


if __name__ == "__main__":
    unittest.main()
