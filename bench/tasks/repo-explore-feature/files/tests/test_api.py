import unittest

from ledgerly.api.handlers import customers, invoices, products
from ledgerly.api.store import STORE

from .helpers import fresh


class ApiTest(unittest.TestCase):
    def setUp(self):
        fresh()
        STORE.clear()
        products.post_product({"sku": "LT-14", "name": "Laptop stand", "unit_price": "49.90"})

    def test_invoice_round_trip(self):
        status, c = customers.post_customer({"name": "Tasca do Zé", "country": "pt"})
        self.assertEqual(status, 201)
        status, inv = invoices.post_invoice({"customer_id": c["id"], "items": [{"sku": "LT-14", "quantity": 3}], "issued_on": "2026-10-01"})
        self.assertEqual(status, 201)
        self.assertEqual(inv["gross_total"], "184.13")
        self.assertEqual(invoices.get_invoice(inv["number"])[1], inv)

    def test_errors(self):
        self.assertEqual(invoices.post_invoice({"customer_id": 99, "items": []})[0], 404)
        self.assertEqual(customers.post_customer({"name": "x"})[0], 400)
