import json
import os
import tempfile
import unittest
from decimal import Decimal

from ledgerly import config
from ledgerly.api.serializers import invoice_to_dict
from ledgerly.rendering.text_renderer import render_invoice_text
from ledgerly.services.invoicing import create_invoice
from ledgerly.services.notes import EXPORT_NOTE

from .helpers import BOOK, ISSUED, LAPTOP, customer, fresh

NOTE = "Reverse charge: VAT to be accounted for by the recipient (Article 196, Directive 2006/112/EC)."


class ReverseChargeTest(unittest.TestCase):
    def setUp(self):
        fresh()
        os.environ.pop("LEDGERLY_CONFIG", None)

    def tearDown(self):
        os.environ.pop("LEDGERLY_CONFIG", None)
        config.reset()

    def assert_reverse(self, inv, vat_id):
        self.assertTrue(all(l.vat_rate == 0 and l.vat == 0 for l in inv.lines), [(l.vat_rate, l.vat) for l in inv.lines])
        self.assertEqual(inv.vat_total, Decimal("0.00"))
        self.assertEqual(inv.gross_total, inv.net_total)
        self.assertIn(NOTE, inv.notes)
        self.assertNotIn(EXPORT_NOTE, inv.notes)
        self.assertTrue(any(n.startswith("Payment due within") for n in inv.notes))
        self.assertIn(vat_id, render_invoice_text(inv))
        self.assertIn(NOTE, render_invoice_text(inv))
        d = invoice_to_dict(inv)
        self.assertIs(d.get("reverse_charge"), True)
        self.assertEqual(d["vat_total"], "0.00")
        json.dumps(d)

    def assert_normal(self, inv, rates):
        self.assertEqual([l.vat_rate for l in inv.lines], [Decimal(r) for r in rates])
        self.assertNotIn(NOTE, inv.notes)
        self.assertNotIn(NOTE, render_invoice_text(inv))
        self.assertIs(invoice_to_dict(inv).get("reverse_charge"), False)

    def test_eu_business_with_valid_vat_id(self):
        self.assert_reverse(create_invoice(customer("ES", vat_id="ESB12345678"), [(LAPTOP, 2), (BOOK, 3)], ISSUED), "ESB12345678")
        self.assert_reverse(create_invoice(customer("DE", vat_id="DE123456789"), [(BOOK, 1)], ISSUED), "DE123456789")

    def test_greek_prefix(self):
        self.assert_reverse(create_invoice(customer("GR", vat_id="EL123456789"), [(LAPTOP, 1)], ISSUED), "EL123456789")

    def test_invalid_or_missing_vat_id_pays_vat(self):
        self.assert_normal(create_invoice(customer("ES", vat_id="ES123"), [(LAPTOP, 1)], ISSUED), ["23"])
        self.assert_normal(create_invoice(customer("DE", vat_id="FR12345678901"), [(LAPTOP, 1)], ISSUED), ["23"])
        self.assert_normal(create_invoice(customer("FR"), [(LAPTOP, 1), (BOOK, 1)], ISSUED), ["23", "6"])

    def test_consumers_pay_vat(self):
        self.assert_normal(create_invoice(customer("ES", kind="consumer", vat_id="ESB12345678"), [(LAPTOP, 1)], ISSUED), ["23"])

    def test_domestic_business(self):
        self.assert_normal(create_invoice(customer("PT", vat_id="PT507172230"), [(LAPTOP, 1)], ISSUED), ["23"])

    def test_outside_the_eu_is_still_an_export(self):
        for country, vat in (("NO", "NO123456789MVA"), ("CH", "CHE123456789"), ("US", None)):
            inv = create_invoice(customer(country, vat_id=vat), [(LAPTOP, 1)], ISSUED)
            self.assertEqual(inv.vat_total, Decimal("0.00"))
            self.assertIn(EXPORT_NOTE, inv.notes, country)
            self.assertNotIn(NOTE, inv.notes, country)
            self.assertIs(invoice_to_dict(inv).get("reverse_charge"), False, country)

    def test_follows_the_company_country(self):
        cfg = {"name": "Azulejo SL", "country": "ES", "vat_id": "ESB87654321", "currency": "EUR", "address": "Calle Mayor 1, Madrid", "payment_terms_days": 15}
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(cfg, f)
        try:
            os.environ["LEDGERLY_CONFIG"] = f.name
            config.reset()
            self.assert_reverse(create_invoice(customer("PT", vat_id="PT507172230"), [(LAPTOP, 1)], ISSUED), "PT507172230")
            self.assert_normal(create_invoice(customer("ES", vat_id="ESB12345678"), [(LAPTOP, 1)], ISSUED), ["21"])
        finally:
            os.unlink(f.name)

    def test_api_flag(self):
        from ledgerly.api.handlers import customers, invoices, products
        from ledgerly.api.store import STORE

        STORE.clear()
        products.post_product({"sku": "LT-14", "name": "Laptop stand", "unit_price": "49.90"})
        _, c = customers.post_customer({"name": "Kaffeehaus GmbH", "country": "de", "vat_id": "DE 123 456 789"})
        status, inv = invoices.post_invoice({"customer_id": c["id"], "items": [{"sku": "LT-14", "quantity": 1}], "issued_on": "2026-10-02"})
        self.assertEqual(status, 201)
        self.assertIs(inv["reverse_charge"], True)
        self.assertEqual(inv["gross_total"], "49.90")
        _, c2 = customers.post_customer({"name": "Tasca", "country": "pt"})
        _, inv2 = invoices.post_invoice({"customer_id": c2["id"], "items": [{"sku": "LT-14", "quantity": 1}], "issued_on": "2026-10-02"})
        self.assertIs(inv2["reverse_charge"], False)
