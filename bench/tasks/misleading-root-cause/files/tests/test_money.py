import unittest

from core.money import cents_from_text, format_eur, share, split_vat


class MoneyTest(unittest.TestCase):
    def test_format(self):
        self.assertEqual(format_eur(1234567), "€12,345.67")
        self.assertEqual(format_eur(5), "€0.05")
        self.assertEqual(format_eur(-1290), "-€12.90")

    def test_parse(self):
        self.assertEqual(cents_from_text("12,50"), 1250)
        self.assertEqual(cents_from_text(" 7.005 "), 701)

    def test_vat(self):
        self.assertEqual(split_vat(5490, 23), (4463, 1027))
        self.assertEqual(split_vat(1290, 19), (1084, 206))

    def test_share(self):
        self.assertEqual(share(2190, 15, 30), 1095)
        self.assertEqual(share(1000, 1, 3), 333)


if __name__ == "__main__":
    unittest.main()
