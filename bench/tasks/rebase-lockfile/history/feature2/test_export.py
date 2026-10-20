import unittest

from ledger import export, parse, report

SAMPLE = """date,description,amount,category
2026-08-01,Energy,-60.00,bills
2026-08-02,Lunch,-12.50,food
2026-08-03,Water,-40.00,bills
2026-08-04,Market,-20.00,food
2026-08-05,Lunch,-17.50,food
2026-08-06,Birthday,-20.00,gifts
"""


class ExportTest(unittest.TestCase):
    def test_csv(self):
        rows = report.summarize(parse.read_csv(SAMPLE))
        self.assertEqual(export.to_csv(rows), "category,total,count\nbills,-100.00,2\nfood,-50.00,3\ngifts,-20.00,1\n")

    def test_csv_quotes_commas(self):
        rows = [{"category": "food, drinks", "total": __import__("decimal").Decimal("-1.5"), "count": 1}]
        self.assertEqual(export.to_csv(rows), 'category,total,count\n"food, drinks",-1.50,1\n')

    def test_xlsx_needs_tabwrite(self):
        try:
            import tabwrite  # noqa: F401
        except ImportError:
            with self.assertRaises(RuntimeError):
                export.to_xlsx([], "/nonexistent/out.xlsx")


if __name__ == "__main__":
    unittest.main()
