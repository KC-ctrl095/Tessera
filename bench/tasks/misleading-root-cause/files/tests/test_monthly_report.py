import unittest

from core.store import Store
from reports.monthly import monthly_report, render

STORE = Store.load()


class MonthlyReportTest(unittest.TestCase):
    # (total in cents, number of payments), checked by finance against the bank statements.

    def test_july_2025(self):
        r = monthly_report(STORE, 2025, 7)
        self.assertEqual((r.total_cents, r.payment_count), (77470, 33))

    def test_september_2026(self):
        r = monthly_report(STORE, 2026, 9)
        self.assertEqual((r.total_cents, r.payment_count), (118590, 53))
        self.assertEqual(r.refunds_cents, -1290)

    def test_october_2026(self):
        r = monthly_report(STORE, 2026, 10)
        self.assertEqual((r.total_cents, r.payment_count), (107780, 46))
        self.assertEqual(r.refunds_cents, -2580)

    def test_render(self):
        text = render(monthly_report(STORE, 2026, 9))
        self.assertEqual(text.splitlines()[0], "Revenue 2026-09 (Lisbon time)")
        self.assertIn("€1,185.90", text)
        self.assertTrue(text.endswith("53 payments from 35 customers"))


if __name__ == "__main__":
    unittest.main()
