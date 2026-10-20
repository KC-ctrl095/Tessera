"""Extra checks the task's check adds next to the project's own tests."""

import unittest
from datetime import date, datetime
from decimal import Decimal

from dockyard import carriers, rates, schedule
from dockyard.catalog import normalize_sku
from dockyard.inventory import Inventory
from dockyard.invoice import build_invoice
from dockyard.models import OrderLine, Parcel
from dockyard.packing import BOXES

from .support import WH, order, small_catalog


class SkuChecks(unittest.TestCase):
    def test_normalize_examples(self):
        cases = [(" ab_100 ", "AB-100"), ("cd . 20", "CD-20"), ("ef--7", "EF-7"), ("gh__ 12", "GH-12"),
                 ("KL-9", "KL-9"), ("mn.402", "MN-402"), ("ab\t104", "AB-104"), ("x - y_z", "X-Y-Z")]
        got = [(raw, normalize_sku(raw)) for raw, _ in cases]
        self.assertEqual(got, cases)

    def test_inventory_matches_hand_typed_skus(self):
        inv = Inventory()
        inv.receive("AMS-1", "AB-104", 4)
        self.assertEqual(inv.available("AMS-1", "ab - 104"), 4)
        inv.reserve("AMS-1", [OrderLine("ab..104", 3)])
        self.assertEqual(inv.available("AMS-1", "AB-104"), 1)

    def test_catalog_finds_hand_typed_skus(self):
        cat = small_catalog()
        self.assertEqual(cat.get("mn  402").sku, "MN-402")
        self.assertIn("gh _ 330", cat)

    def test_invoice_with_hand_typed_skus(self):
        inv = build_invoice(order(("ef..120", 1), (" gh -330 ", 2)), small_catalog())
        self.assertEqual([(l.sku, l.net) for l in inv.lines], [("EF-120", Decimal("4.99")), ("GH-330", Decimal("34.00"))])


class HolidayChecks(unittest.TestCase):
    def test_holidays_are_loaded(self):
        for country, day in [("NL", date(2026, 4, 27)), ("nl", date(2026, 5, 5)), ("BE", date(2026, 7, 21)), ("DE", date(2026, 6, 4))]:
            self.assertIn(day, schedule.load_holidays(country), (country, day))

    def test_holidays_are_not_business_days(self):
        self.assertFalse(schedule.is_business_day(date(2026, 5, 14), "NL"))
        self.assertFalse(schedule.is_business_day(date(2026, 11, 11), "BE"))
        self.assertTrue(schedule.is_business_day(date(2026, 11, 11), "NL"))

    def test_cologne_corpus_christi(self):
        self.assertEqual(schedule.promised_date(datetime(2026, 6, 3, 16, 0), WH["CGN-1"], 1), (date(2026, 6, 5), date(2026, 6, 8)))

    def test_antwerp_national_day(self):
        self.assertEqual(schedule.promised_date(datetime(2026, 7, 20, 17, 0), WH["ANR-1"], 2), (date(2026, 7, 22), date(2026, 7, 24)))

    def test_transit_skips_holidays(self):
        self.assertEqual(schedule.promised_date(datetime(2026, 5, 13, 10, 0), WH["AMS-1"], 2), (date(2026, 5, 13), date(2026, 5, 18)))

    def test_easter_weekend(self):
        self.assertEqual(schedule.promised_date(datetime(2026, 4, 2, 16, 0), WH["RTM-2"], 1), (date(2026, 4, 7), date(2026, 4, 8)))


class BandChecks(unittest.TestCase):
    def test_every_limit_is_inclusive(self):
        for carrier, zones in sorted(rates.CARDS.items()):
            for zone, card in sorted(zones.items()):
                prev = 0
                for limit, price in card:
                    for w in (prev + 1, (prev + limit) // 2, limit):
                        self.assertEqual(rates.band_price(carrier, zone, w), Decimal(price), (carrier, zone, w))
                    prev = limit
                with self.assertRaises(rates.NoRate):
                    rates.band_price(carrier, zone, prev + 1)

    def test_a_full_xl_box_gets_a_quote(self):
        xl = BOXES[-1]
        q = carriers.quote("PostNL", Parcel(xl, ["X"], 20000 - xl.tare_g, 1.0), "NL", "NL", "X/1")
        self.assertEqual(q.price, Decimal("14.50"))


if __name__ == "__main__":
    unittest.main()
