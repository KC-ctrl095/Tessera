"""The worked examples of docs/FARES-SPEC.md §11."""

import json
import unittest
from pathlib import Path

from fares import quote

EXAMPLES = json.loads((Path(__file__).parent / "examples.json").read_text(encoding="utf-8"))


class WorkedExamples(unittest.TestCase):
    def test_examples(self):
        for ex in EXAMPLES:
            with self.subTest(ex["title"]):
                self.assertEqual(quote(ex["booking"]), ex["quote"])

    def test_unknown_route(self):
        with self.assertRaises(ValueError):
            quote({"route": "XX-YY", "date": "2026-05-05", "time": "10:00", "passengers": [{"type": "adult"}]})


if __name__ == "__main__":
    unittest.main()
