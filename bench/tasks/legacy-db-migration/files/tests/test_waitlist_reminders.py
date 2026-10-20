import unittest

from clinic import app, reminders, settings_store, waitlist


class WaitlistTest(unittest.TestCase):
    def setUp(self):
        app.setup()

    def test_next_in_line(self):
        self.assertEqual(waitlist.next_in_line(1), {"id": 4, "patient_id": 2, "added_at": "2026-10-29T08:10"})
        self.assertIsNone(waitlist.next_in_line(5))

    def test_add_twice(self):
        waitlist.add(3, 5)
        with self.assertRaises(Exception):
            waitlist.add(3, 5)


class RemindersTest(unittest.TestCase):
    def setUp(self):
        app.setup()

    def test_due(self):
        due = reminders.due("2026-11-01T18:30")
        self.assertEqual(
            [(r["channel"], r["to"], r["starts_at"]) for r in due],
            [
                ("sms", "+351912400101", "2026-11-02T09:00"),
                ("email", "joana.matos@mail.example", "2026-11-02T09:00"),
                ("email", "leonor.b@mail.example", "2026-11-02T10:30"),
                ("sms", "+351927144380", "2026-11-02T09:00"),
                ("email", "duarte.macedo@mail.example", "2026-11-02T09:00"),
                ("sms", "+351917551032", "2026-11-02T11:00"),
                ("email", "mario.fontes@mail.example", "2026-11-02T11:00"),
            ],
        )


class SettingsTest(unittest.TestCase):
    def setUp(self):
        app.setup()

    def test_defaults_and_overrides(self):
        self.assertEqual(settings_store.get("slot_minutes"), "30")
        self.assertEqual(settings_store.get("reminder_time"), "18:30")
        self.assertIsNone(settings_store.get("nothing"))
        settings_store.set("slot_minutes", 20)
        self.assertEqual(settings_store.get("slot_minutes"), "20")


if __name__ == "__main__":
    unittest.main()
