import unittest

from pagination import paginate


class MorePaginateTest(unittest.TestCase):
    def test_middle_page(self):
        p = paginate(list("abcdefghij"), page=2, per_page=3)
        self.assertEqual(p.items, ["d", "e", "f"])
        self.assertTrue(p.has_next)
        self.assertTrue(p.has_prev)

    def test_exact_multiple(self):
        p = paginate(list(range(40)), page=2, per_page=20)
        self.assertEqual(p.items, list(range(20, 40)))
        self.assertEqual(p.pages, 2)
        self.assertFalse(p.has_next)

    def test_past_the_end(self):
        p = paginate(list(range(5)), page=4, per_page=2)
        self.assertEqual(p.items, [])
        self.assertFalse(p.has_next)

    def test_empty(self):
        p = paginate([], page=1, per_page=10)
        self.assertEqual(p.items, [])
        self.assertEqual(p.pages, 1)
        self.assertFalse(p.has_next)
        self.assertFalse(p.has_prev)

    def test_tuple_input(self):
        p = paginate(tuple(range(7)), page=3, per_page=3)
        self.assertEqual(p.items, [6])

    def test_bad_per_page(self):
        with self.assertRaises(ValueError):
            paginate([1], per_page=0)


if __name__ == "__main__":
    unittest.main()
