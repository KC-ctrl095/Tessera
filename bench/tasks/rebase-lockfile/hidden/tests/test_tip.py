"""Both sides' behaviour at the tip of the rebased branch."""

import io
import os
import tempfile
import unittest
from contextlib import redirect_stdout
from decimal import Decimal

from ledger import cli, export, money, parse, report

SAMPLE = """date,description,amount,category
2026-07-30,Hotel (USD),-80.125,travel
2026-08-01,Bakery,-2.335,food
2026-08-01,Refund,0.005,food
2026-08-02,Books,-15.00,books
2026-08-03,Train,-14.875,travel
2026-08-04,Cinema,-15.00,fun
2026-08-05,Pay,1800.00,income
"""


class TipTest(unittest.TestCase):
    def setUp(self):
        self.tx = parse.read_csv(SAMPLE)

    def test_rows_are_rounded_half_even_sorted_by_total_and_counted(self):
        self.assertEqual(
            report.summarize(self.tx),
            [
                {"category": "travel", "total": Decimal("-95.00"), "count": 2},
                {"category": "books", "total": Decimal("-15.00"), "count": 1},
                {"category": "fun", "total": Decimal("-15.00"), "count": 1},
                {"category": "food", "total": Decimal("-2.33"), "count": 2},
                {"category": "income", "total": Decimal("1800.00"), "count": 1},
            ],
        )

    def test_half_even(self):
        for raw, want in [("0.125", "0.12"), ("0.135", "0.14"), ("-2.335", "-2.34"), ("-2.345", "-2.34")]:
            self.assertEqual(money.round_half_even(Decimal(raw)), Decimal(want))

    def test_csv_follows_the_summary(self):
        self.assertEqual(
            export.to_csv(report.summarize(self.tx)),
            "category,total,count\ntravel,-95.00,2\nbooks,-15.00,1\nfun,-15.00,1\nfood,-2.33,2\nincome,1800.00,1\n",
        )

    def test_render_has_counts(self):
        lines = report.render(report.summarize(self.tx)).splitlines()
        self.assertEqual(lines[0].split(), ["category", "total", "count"])
        self.assertEqual(lines[1].split(), ["travel", "-95.00", "2"])

    def test_cli_csv(self):
        fd, path = tempfile.mkstemp(suffix=".csv")
        try:
            with os.fdopen(fd, "w") as f:
                f.write(SAMPLE)
            out = io.StringIO()
            with redirect_stdout(out):
                cli.main([path, "--csv"])
            self.assertEqual(out.getvalue().splitlines()[1], "travel,-95.00,2")
        finally:
            os.unlink(path)

    def test_monthly_still_rounds_half_up(self):
        self.assertEqual(report.monthly(parse.read_csv("date,description,amount,category\n2026-08-01,x,-0.125,a\n")), {"2026-08": Decimal("-0.13")})


if __name__ == "__main__":
    unittest.main()
