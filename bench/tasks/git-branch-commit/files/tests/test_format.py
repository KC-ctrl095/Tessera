import unittest

from porchlight.format import MAX_LEN, format_event, wants


class FormatTest(unittest.TestCase):
    def test_passed(self):
        e = {"status": "passed", "project": "api", "number": 41, "branch": "main"}
        self.assertEqual(format_event(e), "[OK] api #41 on main")

    def test_failed_with_summary(self):
        e = {"status": "failed", "project": "web", "number": 7, "summary": "2 tests failed"}
        self.assertEqual(format_event(e), "[FAILED] web #7: 2 tests failed")

    def test_long_message_is_cut(self):
        e = {"status": "passed", "project": "api", "number": 1, "summary": "x" * 5000}
        self.assertEqual(len(format_event(e)), MAX_LEN)

    def test_only_failures(self):
        room = {"name": "alerts", "only_failures": True}
        self.assertTrue(wants(room, {"status": "failed"}))
        self.assertFalse(wants(room, {"status": "passed"}))


if __name__ == "__main__":
    unittest.main()
