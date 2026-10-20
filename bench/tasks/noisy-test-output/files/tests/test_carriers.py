import hashlib
import unittest
from decimal import Decimal

from dockyard import carriers, rates
from dockyard.fixtures import make_context, make_orders
from dockyard.models import Parcel
from dockyard.packing import BOXES, pack

S, M, L, XL = BOXES


def parcel(box, content_g):
    return Parcel(box, ["X"], content_g, 100.0)


class QuoteTest(unittest.TestCase):
    def test_cheapest_domestic_standard(self):
        q = carriers.choose(parcel(S, 480), "NL", "NL", "standard", "T/1")
        self.assertEqual((q.carrier, q.price, q.days), ("PostNL", Decimal("5.25"), 1))

    def test_cross_border_standard(self):
        q = carriers.choose(parcel(S, 480), "NL", "BE", "standard", "T/2")
        self.assertEqual((q.carrier, q.price, q.days), ("DHL", Decimal("9.10"), 2))

    def test_heavy_parcel_goes_to_the_cheapest_slow_carrier(self):
        q = carriers.choose(parcel(L, 8620), "NL", "NL", "standard", "T/3")
        self.assertEqual((q.carrier, q.price), ("DPD", Decimal("9.60")))

    def test_express_needs_next_day(self):
        q = carriers.choose(parcel(L, 8620), "NL", "NL", "express", "T/4")
        self.assertEqual((q.carrier, q.price, q.days), ("PostNL", Decimal("9.80"), 1))

    def test_no_express_across_borders(self):
        with self.assertLogs("dockyard.carriers", "WARNING"):
            self.assertIsNone(carriers.choose(parcel(S, 480), "NL", "DE", "express", "T/5"))

    def test_timeout_falls_back_to_the_rate_card(self):
        ref = next(r for r in (f"T/{i}" for i in range(200)) if int(hashlib.sha1(r.encode()).hexdigest(), 16) % 7 == 0)
        with self.assertLogs("dockyard.carriers", "ERROR"):
            q = carriers.quote("DPD", parcel(S, 480), "NL", "NL", ref)
        self.assertEqual(q.price, Decimal("6.40"))

    def test_too_heavy_gets_no_quote(self):
        self.assertIsNone(carriers.quote("PostNL", parcel(XL, 24000), "NL", "NL", "T/7"))

    def test_quotes_use_the_rate_card(self):
        for c in sorted(rates.CARDS):
            q = carriers.quote(c, parcel(M, 3000), "NL", "BE", f"T/8/{c}")
            self.assertEqual(q.price, rates.band_price(c, "eu1", 3210))


class SampleDayTest(unittest.TestCase):
    def test_every_parcel_of_a_sample_day_gets_a_standard_quote(self):
        ctx = make_context(3, stock_scale=4)
        for o in make_orders(ctx.catalog, 160, seed=3):
            items = [(ctx.catalog.get(l.sku).sku, ctx.catalog.get(l.sku)) for l in o.lines for _ in range(l.qty)]
            for i, p in enumerate(pack(items), 1):
                q = carriers.choose(p, "NL", o.country, "standard", f"{o.id}/{i}")
                self.assertIsNotNone(q)
                self.assertLessEqual(q.days, 3)


if __name__ == "__main__":
    unittest.main()
