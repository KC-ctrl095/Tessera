import unittest
from datetime import date, datetime

from dockyard import schedule

from .support import WH


class ShipDateTest(unittest.TestCase):
    def test_before_cutoff_ships_the_same_day(self):
        self.assertEqual(schedule.ship_date(datetime(2026, 3, 3, 10, 0), WH["AMS-1"]), date(2026, 3, 3))

    def test_at_cutoff_ships_the_next_day(self):
        self.assertEqual(schedule.ship_date(datetime(2026, 3, 3, 15, 0), WH["AMS-1"]), date(2026, 3, 4))

    def test_friday_evening_ships_on_monday(self):
        self.assertEqual(schedule.ship_date(datetime(2026, 3, 6, 18, 0), WH["AMS-1"]), date(2026, 3, 9))

    def test_saturday_ships_on_monday(self):
        self.assertEqual(schedule.ship_date(datetime(2026, 3, 7, 9, 0), WH["RTM-2"]), date(2026, 3, 9))

    def test_eindhoven_cuts_off_at_two(self):
        self.assertEqual(schedule.ship_date(datetime(2026, 3, 3, 14, 30), WH["EIN-1"]), date(2026, 3, 4))

    def test_antwerp_cuts_off_at_four(self):
        self.assertEqual(schedule.ship_date(datetime(2026, 3, 3, 15, 30), WH["ANR-1"]), date(2026, 3, 3))


class PromiseTest(unittest.TestCase):
    def test_promise_counts_business_days(self):
        self.assertEqual(schedule.promised_date(datetime(2026, 3, 3, 10, 0), WH["AMS-1"], 2), (date(2026, 3, 3), date(2026, 3, 5)))

    def test_promise_skips_the_weekend(self):
        self.assertEqual(schedule.add_business_days(date(2026, 3, 5), 2, "NL"), date(2026, 3, 9))

    def test_zero_transit(self):
        self.assertEqual(schedule.promised_date(datetime(2026, 3, 4, 9, 0), WH["CGN-1"], 0), (date(2026, 3, 4), date(2026, 3, 4)))

    def test_weekends_are_not_business_days(self):
        self.assertFalse(schedule.is_business_day(date(2026, 3, 7), "NL"))
        self.assertTrue(schedule.is_business_day(date(2026, 3, 9), "NL"))

    def test_next_business_day_after_friday(self):
        self.assertEqual(schedule.next_business_day(date(2026, 3, 6), "BE"), date(2026, 3, 9))

    def test_kings_day_is_not_a_shipping_day(self):
        # Friday 24 April after the cut-off; Monday 27 April is King's Day in the Netherlands.
        shipped, due = schedule.promised_date(datetime(2026, 4, 24, 16, 30), WH["AMS-1"], 1)
        self.assertEqual((shipped, due), (date(2026, 4, 28), date(2026, 4, 29)))


if __name__ == "__main__":
    unittest.main()
