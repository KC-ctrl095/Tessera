import unittest

from durations import parse_duration


class SpecTest(unittest.TestCase):
    def ok(self, text, seconds):
        with self.subTest(text=text):
            got = parse_duration(text)
            self.assertIsInstance(got, (int, float))
            self.assertAlmostEqual(got, seconds, places=6)

    def bad(self, text):
        with self.subTest(text=text), self.assertRaises(ValueError):
            parse_duration(text)

    def test_units(self):
        self.ok("3d", 259200)
        self.ok("5h", 18000)
        self.ok("7m", 420)
        self.ok("9s", 9)
        self.ok("250ms", 0.25)

    def test_milliseconds_are_not_minutes(self):
        self.ok("1m5s", 65)
        self.ok("1ms", 0.001)
        self.ok("2m500ms", 120.5)
        self.ok("1s1ms", 1.001)

    def test_decimals(self):
        self.ok("1.5h", 5400)
        self.ok("0.25m", 15)
        self.ok("1.5s250ms", 1.75)
        self.bad("1.h")
        self.bad(".5h")
        self.bad("1..5h")
        self.bad("1.5.5h")

    def test_spaces(self):
        self.ok("  1h 30m  ", 5400)
        self.ok("1h   30m 15s", 5415)
        self.ok("1d2h3m4s5ms", 93784.005)
        self.bad("1 h")
        self.bad("1h 30 m")

    def test_case(self):
        self.ok("1H30M", 5400)
        self.ok("2MS", 0.002)
        self.ok("1D", 86400)

    def test_order_and_repeats(self):
        self.bad("30m1h")
        self.bad("1m1m")
        self.bad("1s1m")
        self.bad("5ms1s")
        self.bad("1h 2h")
        self.ok("1d1s", 86401)

    def test_sign(self):
        self.ok("-1h30m", -5400)
        self.ok("-90s", -90)
        self.ok(" -2m", -120)
        self.bad("1h-30m")
        self.bad("--1h")
        self.bad("+1h")
        self.bad("-")

    def test_zero_and_bare_numbers(self):
        self.ok("0", 0)
        self.ok("0s", 0)
        self.ok("0h0m", 0)
        self.bad("10")
        self.bad("1h30")
        self.bad("00")

    def test_garbage(self):
        for t in ["", "   ", "h", "1hh", "1h,30m", "1w", "1h30m!", "1e3s", "1_000s", "abc1h", "ms"]:
            self.bad(t)

    def test_type(self):
        self.assertIsInstance(parse_duration("1h"), float)


if __name__ == "__main__":
    unittest.main()
