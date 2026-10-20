import unittest
from datetime import date
from decimal import Decimal

from ledger import money, parse


class ParseTest(unittest.TestCase):
    def test_amounts(self):
        self.assertEqual(money.parse_amount("12.50"), Decimal("12.50"))
        self.assertEqual(money.parse_amount(" -3.335 "), Decimal("-3.335"))
        self.assertEqual(money.parse_amount("1,234.50"), Decimal("1234.50"))
        self.assertEqual(money.parse_amount("(12.50)"), Decimal("-12.50"))

    def test_bad_amount(self):
        with self.assertRaises(ValueError):
            money.parse_amount("twelve")

    def test_read_csv(self):
        rows = parse.read_csv("date,description,amount,category\n2026-08-01, Bakery ,-3.20,\n")
        self.assertEqual(rows, [parse.Transaction(date(2026, 8, 1), "Bakery", Decimal("-3.20"), "uncategorised")])


if __name__ == "__main__":
    unittest.main()
