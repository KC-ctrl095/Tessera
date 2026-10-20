import unittest
from datetime import datetime, timedelta

from api.usage import UsageMeter, hourly_counts, usage_window
from utils.periods import day_length


class UsageTest(unittest.TestCase):
    def test_window_winter(self):
        self.assertEqual(usage_window(datetime(2026, 2, 3, 14, 0)), (datetime(2026, 2, 3), datetime(2026, 2, 4)))

    def test_window_summer(self):
        # 23:10 UTC on 9 June is 00:10 on 10 June in Lisbon.
        self.assertEqual(
            usage_window(datetime(2026, 6, 9, 23, 10)), (datetime(2026, 6, 9, 23, 0), datetime(2026, 6, 10, 23, 0))
        )

    def test_short_day_in_march(self):
        self.assertEqual(day_length(datetime(2026, 3, 29).date()), timedelta(hours=23))
        self.assertEqual(usage_window(datetime(2026, 3, 29, 12)), (datetime(2026, 3, 29), datetime(2026, 3, 29, 23)))

    def test_quota(self):
        meter = UsageMeter(quota=3)
        now = datetime(2026, 7, 1, 10, 0)
        self.assertTrue(all(meter.allow(5, now + timedelta(minutes=i)) for i in range(3)))
        self.assertFalse(meter.allow(5, now + timedelta(minutes=4)))
        self.assertTrue(meter.allow(6, now))
        # The quota resets at Lisbon midnight (23:00 UTC in summer).
        self.assertEqual(meter.resets_at(now), datetime(2026, 7, 1, 23, 0))
        self.assertTrue(meter.allow(5, datetime(2026, 7, 1, 23, 0)))

    def test_hourly(self):
        calls = [datetime(2026, 7, 1, 8, 5), datetime(2026, 7, 1, 8, 50), datetime(2026, 7, 1, 22, 59)]
        self.assertEqual(hourly_counts(calls, datetime(2026, 7, 1).date()), {9: 2, 23: 1})


if __name__ == "__main__":
    unittest.main()
