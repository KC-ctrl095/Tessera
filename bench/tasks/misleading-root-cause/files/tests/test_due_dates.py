import unittest
from datetime import date, datetime

from billing.due_dates import due_at, due_day, is_overdue
from billing.dunning import reminder_times


class DueDatesTest(unittest.TestCase):
    def test_winter(self):
        issued = datetime(2026, 1, 10, 15, 0)
        self.assertEqual(due_day(issued), date(2026, 1, 24))
        self.assertEqual(due_at(issued), datetime(2026, 1, 24, 23, 59, 59))

    def test_summer(self):
        # 23:30 UTC on 3 July is already 4 July in Lisbon.
        issued = datetime(2026, 7, 3, 23, 30)
        self.assertEqual(due_day(issued), date(2026, 7, 18))
        self.assertEqual(due_at(issued), datetime(2026, 7, 18, 22, 59, 59))

    def test_due_in_late_october(self):
        issued = datetime(2026, 10, 14, 10, 0)
        self.assertEqual(due_at(issued), datetime(2026, 10, 28, 23, 59, 59))
        self.assertFalse(is_overdue(issued, datetime(2026, 10, 28, 23, 30)))
        self.assertTrue(is_overdue(issued, datetime(2026, 10, 29, 0, 0)))

    def test_reminders(self):
        issued = datetime(2026, 5, 4, 12, 0)
        self.assertEqual(
            reminder_times(issued),
            [datetime(2026, 5, 19, 8, 0), datetime(2026, 5, 25, 8, 0), datetime(2026, 6, 1, 8, 0)],
        )
        issued = datetime(2026, 1, 20, 12, 0)
        self.assertEqual(reminder_times(issued)[0], datetime(2026, 2, 4, 9, 0))


if __name__ == "__main__":
    unittest.main()
