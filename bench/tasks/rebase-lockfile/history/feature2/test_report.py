import unittest
from decimal import Decimal

from ledger import parse, report

SAMPLE = """date,description,amount,category
2026-08-01,Albert Heijn,-23.40,groceries
2026-08-01,Salary,2500.00,income
2026-08-02,Eurostar (GBP),-12.105,travel
2026-08-03,Rent,-950.00,rent
2026-08-09,Jumbo,-17.80,groceries
2026-08-14,Pizzeria (CHF),-18.625,eating out
2026-08-20,Thalys,-30.000,travel
2026-09-02,Albert Heijn,-20.00,groceries
"""


class ReportTest(unittest.TestCase):
    def setUp(self):
        self.tx = parse.read_csv(SAMPLE)

    def test_summarize(self):
        self.assertEqual(
            report.summarize(self.tx),
            [
                {"category": "eating out", "total": Decimal("-18.63"), "count": 1},
                {"category": "groceries", "total": Decimal("-61.20"), "count": 3},
                {"category": "income", "total": Decimal("2500.00"), "count": 1},
                {"category": "rent", "total": Decimal("-950.00"), "count": 1},
                {"category": "travel", "total": Decimal("-42.11"), "count": 2},
            ],
        )

    def test_monthly(self):
        self.assertEqual(report.monthly(self.tx), {"2026-08": Decimal("1448.07"), "2026-09": Decimal("-20.00")})

    def test_render(self):
        text = report.render(report.summarize(self.tx)[:2])
        self.assertEqual(text.splitlines(), ["category         total  count", "eating out      -18.63      1", "groceries       -61.20      3"])


if __name__ == "__main__":
    unittest.main()
