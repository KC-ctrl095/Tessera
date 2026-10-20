import unittest

from cache import TTLCache


class Clock:
    def __init__(self):
        self.now = 1000.0

    def __call__(self):
        return self.now


class TTLCacheTest(unittest.TestCase):
    def test_get_and_expiry(self):
        clock = Clock()
        c = TTLCache(maxsize=3, ttl=10, clock=clock)
        c.set("a", 1)
        self.assertEqual(c.get("a"), 1)
        clock.now += 11
        self.assertIsNone(c.get("a"))

    def test_reading_keeps_an_entry(self):
        c = TTLCache(maxsize=2, ttl=100, clock=Clock())
        c.set("a", 1)
        c.set("b", 2)
        c.get("a")
        c.set("c", 3)  # the cache is full: "b" is the least recently used
        self.assertEqual(c.get("a"), 1)
        self.assertIsNone(c.get("b"))

    def test_zero_is_a_value(self):
        c = TTLCache(clock=Clock())
        c.set("stock", 0)
        self.assertEqual(c.get("stock", "missing"), 0)


if __name__ == "__main__":
    unittest.main()
