import unittest

from ledgerly.rendering.text_renderer import render_invoice_text
from ledgerly.services.invoicing import create_invoice

from .helpers import ISSUED, LAPTOP, customer, fresh


class RenderingTest(unittest.TestCase):
    def setUp(self):
        fresh()

    def test_text(self):
        text = render_invoice_text(create_invoice(customer("ES", kind="consumer"), [(LAPTOP, 1)], ISSUED))
        self.assertIn("INVOICE FT 2026/00001", text)
        self.assertIn("Spain", text)
        self.assertIn("Total:", text)
        self.assertIn("Payment due within 30 days", text)
