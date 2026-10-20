import random
import unittest

from cache import TTLCache


class Clock:
    def __init__(self):
        self.now = 50.0

    def __call__(self):
        return self.now


class SpecTest(unittest.TestCase):
    def setUp(self):
        self.clock = Clock()

    def make(self, maxsize=3, ttl=10):
        return TTLCache(maxsize=maxsize, ttl=ttl, clock=self.clock)

    def test_expiry_boundary(self):
        c = self.make()
        c.set("a", "x")
        self.clock.now += 9.999
        self.assertEqual(c.get("a"), "x")
        self.clock.now = 60.0
        self.assertEqual(c.get("a", "gone"), "gone")
        self.assertNotIn("a", c)

    def test_falsy_values(self):
        c = self.make(maxsize=5)
        for k, v in [("zero", 0), ("empty", ""), ("none", None), ("false", False), ("list", [])]:
            c.set(k, v)
        for k, v in [("zero", 0), ("empty", ""), ("none", None), ("false", False), ("list", [])]:
            self.assertEqual(c.get(k, "default"), v, k)
            self.assertIn(k, c)

    def test_replace_restarts_ttl(self):
        c = self.make()
        c.set("a", 1)
        self.clock.now += 8
        c.set("a", 2)
        self.clock.now += 8
        self.assertEqual(c.get("a"), 2)
        self.clock.now += 2
        self.assertIsNone(c.get("a"))

    def test_replace_counts_as_use(self):
        c = self.make(maxsize=2, ttl=100)
        c.set("a", 1)
        c.set("b", 2)
        c.set("a", 3)
        c.set("c", 4)
        self.assertEqual(c.get("a"), 3)
        self.assertNotIn("b", c)

    def test_expired_entries_go_first(self):
        c = self.make(maxsize=3, ttl=10)
        c.set("old", 1)
        self.clock.now += 5
        c.set("b", 2)
        c.set("c", 3)
        self.clock.now += 6  # "old" expired, b and c still live
        c.set("d", 4)
        self.assertEqual((c.get("b"), c.get("c"), c.get("d")), (2, 3, 4))
        self.assertEqual(len(c), 3)

    def test_len_and_contains_skip_expired(self):
        c = self.make(maxsize=5, ttl=10)
        c.set("a", 1)
        self.clock.now += 5
        c.set("b", 2)
        self.assertEqual(len(c), 2)
        self.clock.now += 6
        self.assertEqual(len(c), 1)
        self.assertNotIn("a", c)
        self.assertIn("b", c)

    def test_delete(self):
        c = self.make()
        c.set("a", 1)
        self.assertTrue(c.delete("a"))
        self.assertFalse(c.delete("a"))
        c.set("b", 2)
        self.clock.now += 20
        self.assertFalse(c.delete("b"))
        self.assertEqual(len(c), 0)

    def test_contains_does_not_count_as_use(self):
        c = self.make(maxsize=2, ttl=100)
        c.set("a", 1)
        c.set("b", 2)
        self.assertIn("a", c)
        c.set("c", 3)
        self.assertNotIn("a", c)
        self.assertIn("b", c)

    def test_bad_maxsize(self):
        with self.assertRaises(ValueError):
            TTLCache(maxsize=0)

    def test_against_a_model(self):
        rng = random.Random(5)
        for trial in range(200):
            clock = Clock()
            maxsize, ttl = rng.randint(1, 4), rng.choice([3, 5, 10])
            c = TTLCache(maxsize=maxsize, ttl=ttl, clock=clock)
            model = {}  # key -> [expires, value, last_use]
            tick = 0
            for step in range(40):
                clock.now += rng.choice([0, 0.5, 1, 2, 4])
                tick += 1
                now = clock.now
                for k in [k for k, e in model.items() if now >= e[0]]:
                    del model[k]
                key = rng.choice("abcdef")
                op = rng.random()
                if op < 0.45:
                    if key not in model and len(model) >= maxsize:
                        lru = min(model, key=lambda k: model[k][2])
                        del model[lru]
                    model[key] = [now + ttl, step, tick]
                    c.set(key, step)
                elif op < 0.85:
                    want = model[key][1] if key in model else "-"
                    if key in model:
                        model[key][2] = tick
                    self.assertEqual(c.get(key, "-"), want, f"trial {trial} step {step} get {key}")
                else:
                    self.assertEqual(c.delete(key), key in model, f"trial {trial} step {step} delete {key}")
                    model.pop(key, None)
                self.assertEqual(len(c), len(model), f"trial {trial} step {step} len")


if __name__ == "__main__":
    unittest.main()
