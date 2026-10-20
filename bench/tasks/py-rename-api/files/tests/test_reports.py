import unittest

from shopkit import orders_for, spending_report


class ReportsTest(unittest.TestCase):
    def test_orders(self):
        self.assertEqual([o.id for o in orders_for(1)], [100, 102])
        self.assertEqual(orders_for(1)[0].customer.email, "ada@example.org")

    def test_report(self):
        self.assertEqual(
            spending_report(),
            ["Ada Brandt <ada@example.org>: 34.29", "Joao Pires <joao@example.org>: 12.00"],
        )
