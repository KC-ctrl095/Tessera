import unittest

from pagination import paginate


class PaginateTest(unittest.TestCase):
    def test_first_page(self):
        p = paginate(list(range(50)), page=1, per_page=20)
        self.assertEqual(p.items, list(range(20)))
        self.assertTrue(p.has_next)
        self.assertFalse(p.has_prev)

    def test_last_page(self):
        p = paginate(list(range(50)), page=3, per_page=20)
        self.assertEqual(p.items, list(range(40, 50)))
        self.assertEqual(p.pages, 3)
        self.assertFalse(p.has_next)

    def test_bad_page(self):
        with self.assertRaises(ValueError):
            paginate([1, 2, 3], page=0)


if __name__ == "__main__":
    unittest.main()
