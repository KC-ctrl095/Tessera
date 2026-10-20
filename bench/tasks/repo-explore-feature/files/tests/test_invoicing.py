import unittest
from decimal import Decimal

from ledgerly.services.invoicing import create_invoice
from ledgerly.services.notes import EXPORT_NOTE

from .helpers import BOOK, ISSUED, LAPTOP, customer, fresh


class InvoicingTest(unittest.TestCase):
    def setUp(self):
        fresh()

    def test_domestic_business(self):
        inv = create_invoice(customer("PT", vat_id="PT507172230"), [(LAPTOP, 2), (BOOK, 1)], ISSUED)
        self.assertEqual([l.vat_rate for l in inv.lines], [Decimal("23"), Decimal("6")])
        self.assertEqual(inv.net_total, Decimal("119.80"))
        self.assertEqual(inv.vat_total, Decimal("24.15"))
        self.assertEqual(inv.number, "FT 2026/00001")

    def test_export(self):
        inv = create_invoice(customer("US"), [(LAPTOP, 1)], ISSUED)
        self.assertEqual(inv.vat_total, Decimal("0.00"))
        self.assertIn(EXPORT_NOTE, inv.notes)

    def test_eu_consumer_pays_portuguese_vat(self):
        inv = create_invoice(customer("FR", kind="consumer"), [(LAPTOP, 1)], ISSUED)
        self.assertEqual(inv.lines[0].vat_rate, Decimal("23"))

    def test_volume_discount(self):
        inv = create_invoice(customer("PT"), [(BOOK, 20)], ISSUED)
        self.assertEqual(inv.lines[0].unit_price, Decimal("19.00"))
