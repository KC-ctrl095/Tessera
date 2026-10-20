import unittest

import rating


class RatingTest(unittest.TestCase):
    def test_eu_is_free(self):
        self.assertEqual(rating.rate_event("pt", "voice", 300), 0.0)
        self.assertEqual(rating.rate_event("de", "data", 50), 0.0)

    def test_plans(self):
        self.assertEqual(rating.plan_label("personal", "Silver"), "Personal Silver")
        with self.assertRaises(ValueError):
            rating.rate_event("pt", "voice", 10, "personal", "Platinum")

    def test_every_country_has_every_service(self):
        for code in rating.COUNTRIES:
            for svc in ("voice", "voice_in", "sms", "mms", "data", "video"):
                self.assertIn((code, svc), rating.RULES)


if __name__ == "__main__":
    unittest.main()
