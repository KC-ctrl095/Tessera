import unittest
from datetime import datetime

from api.handlers import Api
from core.store import Store

STORE = Store.load()
KEY = {"Authorization": "Key demo-key-casa-do-grao"}  # customer 3


class ApiTest(unittest.TestCase):
    def setUp(self):
        self.api = Api(STORE)

    def first_payment_of(self, customer_id, month):
        return next(p for p in STORE.payments if p.customer_id == customer_id and p.paid_at.month == month)

    def test_needs_key(self):
        self.assertEqual(self.api.handle("GET", "/usage", {}, datetime(2026, 7, 1))[0], 401)

    def test_payment_in_summer(self):
        p = self.first_payment_of(3, 7)
        status, body = self.api.handle("GET", f"/payments/{p.id}", KEY, datetime(2026, 7, 1))
        self.assertEqual(status, 200)
        self.assertTrue(body["paid_at"].endswith("+01:00"))
        self.assertEqual(body["amount_cents"], p.amount_cents)

    def test_other_customers_payment(self):
        p = self.first_payment_of(11, 5)
        self.assertEqual(self.api.handle("GET", f"/payments/{p.id}", KEY, datetime(2026, 7, 1))[0], 404)

    def test_invoice(self):
        p = self.first_payment_of(3, 2)
        status, body = self.api.handle("GET", f"/payments/{p.id}/invoice", KEY, datetime(2026, 7, 1))
        self.assertEqual(status, 200)
        self.assertEqual(body["vat_rate"], 23)
        self.assertTrue(body["due_at"].endswith("T23:59:59+00:00"))

    def test_monthly_report(self):
        status, body = self.api.handle("GET", "/reports/monthly/2025-07", KEY, datetime(2026, 7, 1))
        self.assertEqual((status, body["total_cents"], body["payments"]), (200, 77470, 33))
        self.assertEqual(self.api.handle("GET", "/reports/monthly/2025-13", KEY, datetime(2026, 7, 1))[0], 400)

    def test_usage(self):
        now = datetime(2026, 1, 12, 10, 0)
        self.api.handle("GET", "/usage", KEY, now)
        status, body = self.api.handle("GET", "/usage", KEY, now)
        self.assertEqual(body, {"used": 2, "remaining": 498, "resets_at": "2026-01-13T00:00:00+00:00"})


if __name__ == "__main__":
    unittest.main()
