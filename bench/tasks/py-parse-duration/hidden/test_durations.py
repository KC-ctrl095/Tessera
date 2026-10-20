import unittest

from durations import parse_duration


class ParseDurationTest(unittest.TestCase):
    def test_simple(self):
        self.assertEqual(parse_duration("90s"), 90)
        self.assertEqual(parse_duration("2m"), 120)

    def test_combined(self):
        self.assertEqual(parse_duration("1h30m"), 5400)
        self.assertEqual(parse_duration("1d 2h"), 93600)

    def test_invalid(self):
        for bad in ["", "abc", "10x"]:
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                parse_duration(bad)


if __name__ == "__main__":
    unittest.main()
