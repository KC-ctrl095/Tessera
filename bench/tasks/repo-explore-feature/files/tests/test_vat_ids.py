import unittest

from ledgerly.validation.vat_ids import is_valid_vat_id


class VatIdTest(unittest.TestCase):
    def test_formats(self):
        self.assertTrue(is_valid_vat_id("DE", "DE123456789"))
        self.assertTrue(is_valid_vat_id("ES", "ESB12345678"))
        self.assertFalse(is_valid_vat_id("DE", "DE12345"))
        self.assertFalse(is_valid_vat_id("NO", "NO123456789MVA"))

    def test_portuguese_check_digit(self):
        self.assertTrue(is_valid_vat_id("PT", "PT 507 172 230"))
        self.assertFalse(is_valid_vat_id("PT", "PT507172231"))
