import unittest
from datetime import datetime, time

from core.models import Subscription
from core.renewals import next_renewal, renewals_between


def sub(day, hhmm, started=datetime(2025, 1, 1), cancelled=None):
    return Subscription(1, 1, "filter", started, day, time(*hhmm), cancelled)


class RenewalsTest(unittest.TestCase):
    def test_winter_and_summer(self):
        s = sub(15, (9, 30))
        self.assertEqual(next_renewal(s, datetime(2026, 1, 1)), datetime(2026, 1, 15, 9, 30))
        self.assertEqual(next_renewal(s, datetime(2026, 6, 15, 8, 30)), datetime(2026, 7, 15, 8, 30))

    def test_short_months(self):
        s = sub(31, (12, 0))
        self.assertEqual(next_renewal(s, datetime(2026, 2, 1)), datetime(2026, 2, 28, 12, 0))
        self.assertEqual(next_renewal(s, datetime(2028, 2, 1)), datetime(2028, 2, 29, 12, 0))
        self.assertEqual(next_renewal(s, datetime(2026, 6, 1)), datetime(2026, 6, 30, 11, 0))

    def test_cancelled(self):
        s = sub(10, (8, 0), cancelled=datetime(2026, 5, 1))
        self.assertIsNone(next_renewal(s, datetime(2026, 4, 20)))

    def test_between(self):
        s = sub(5, (18, 0))
        got = renewals_between(s, datetime(2026, 1, 1), datetime(2026, 4, 1))
        self.assertEqual(got, [datetime(2026, 1, 5, 18), datetime(2026, 2, 5, 18), datetime(2026, 3, 5, 18)])


if __name__ == "__main__":
    unittest.main()
