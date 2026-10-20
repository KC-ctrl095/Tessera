import unittest
from datetime import date, datetime, timedelta

from utils.tz import last_sunday, local_date, to_local, to_utc, utc_offset


class TzTest(unittest.TestCase):
    def test_winter_and_summer(self):
        self.assertEqual(utc_offset(datetime(2026, 1, 15, 12)), timedelta(0))
        self.assertEqual(utc_offset(datetime(2026, 7, 15, 12)), timedelta(hours=1))
        self.assertEqual(to_local(datetime(2026, 7, 15, 23, 30)), datetime(2026, 7, 16, 0, 30))
        self.assertEqual(local_date(datetime(2026, 7, 15, 23, 30)), date(2026, 7, 16))

    def test_round_trip(self):
        for start in (datetime(2026, 1, 10), datetime(2026, 7, 10)):
            for h in range(0, 72):
                t = start + timedelta(hours=h, minutes=17)
                self.assertEqual(to_utc(to_local(t)), t)

    def test_last_sunday(self):
        self.assertEqual(last_sunday(2024, 3), date(2024, 3, 31))
        self.assertEqual(last_sunday(2025, 10), date(2025, 10, 26))
        self.assertEqual(last_sunday(2027, 3), date(2027, 3, 28))

    def test_spring_change(self):
        self.assertEqual(to_local(datetime(2026, 3, 29, 0, 59)), datetime(2026, 3, 29, 0, 59))
        self.assertEqual(to_local(datetime(2026, 3, 29, 1, 0)), datetime(2026, 3, 29, 2, 0))
        # 01:30 doesn't exist that night; it's read as if the clocks hadn't changed yet.
        self.assertEqual(to_utc(datetime(2026, 3, 29, 1, 30)), datetime(2026, 3, 29, 1, 30))

    def test_autumn_change(self):
        self.assertEqual(to_local(datetime(2025, 10, 26, 0, 30)), datetime(2025, 10, 26, 1, 30))
        self.assertEqual(to_local(datetime(2025, 10, 26, 1, 30)), datetime(2025, 10, 26, 1, 30))
        # 01:30 happens twice; the first one is meant.
        self.assertEqual(to_utc(datetime(2025, 10, 26, 1, 30)), datetime(2025, 10, 26, 0, 30))


if __name__ == "__main__":
    unittest.main()
