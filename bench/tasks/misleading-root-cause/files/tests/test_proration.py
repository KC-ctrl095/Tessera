import unittest
from datetime import datetime

from billing.proration import days_between, prorate


class ProrationTest(unittest.TestCase):
    def test_days_between(self):
        self.assertEqual(days_between(datetime(2026, 6, 1, 10), datetime(2026, 7, 1, 10)), 30)
        # 23:30 UTC on 14 June is already 15 June in Lisbon.
        self.assertEqual(days_between(datetime(2026, 6, 14, 23, 30), datetime(2026, 6, 30, 23, 0)), 16)

    def test_upgrade_mid_june(self):
        got = prorate(2190, 5490, datetime(2026, 6, 16, 9), datetime(2026, 6, 1, 9), datetime(2026, 7, 1, 9))
        self.assertEqual(got, {"days_total": 30, "days_left": 15, "credit": 1095, "charge": 2745, "due": 1650})

    def test_change_outside_period(self):
        with self.assertRaises(ValueError):
            prorate(2190, 5490, datetime(2026, 7, 2, 9), datetime(2026, 6, 1, 9), datetime(2026, 7, 1, 9))


if __name__ == "__main__":
    unittest.main()
