import unittest

from clinic import app, appointments, slots
from clinic.errors import Conflict, NotFound


class BookingTest(unittest.TestCase):
    def setUp(self):
        app.setup()

    def test_book_a_free_slot(self):
        free = slots.free(4, "2026-11-03")
        self.assertEqual(free[0], {"id": free[0]["id"], "starts_at": "2026-11-03T09:00", "minutes": 30})
        appt = appointments.book(10, free[0]["id"], "rash")
        self.assertNotIn(free[0]["id"], [s["id"] for s in slots.free(4, "2026-11-03")])
        day = appointments.day_list(4, "2026-11-03")
        self.assertEqual(day, [{"id": appt, "time": "09:00", "patient": "Carlos Pinheiro", "reason": "rash", "status": "booked"}])

    def test_taken_slot(self):
        taken = slots.free(1, "2026-11-02")[0]["id"]
        appointments.book(3, taken)
        with self.assertRaises(Conflict):
            appointments.book(4, taken)

    def test_unknown_slot_or_patient(self):
        with self.assertRaises(NotFound):
            appointments.book(3, 99999)
        with self.assertRaises(NotFound):
            appointments.book(999, slots.free(1, "2026-11-02")[0]["id"])

    def test_cancel(self):
        day = appointments.day_list(1, "2026-11-02")
        self.assertEqual([(a["time"], a["patient"]) for a in day], [("09:00", "Joana Matos"), ("10:30", "Leonor Bastos")])
        before = len(slots.free(1, "2026-11-02"))
        appointments.cancel(day[1]["id"], "travelling")
        self.assertEqual(len(slots.free(1, "2026-11-02")), before + 1)
        self.assertEqual(appointments.day_list(1, "2026-11-02")[1]["status"], "cancelled")
        with self.assertRaises(Conflict):
            appointments.cancel(day[1]["id"])


class SlotsTest(unittest.TestCase):
    def setUp(self):
        app.setup()

    def test_generate_day(self):
        self.assertEqual(slots.generate_day(6, "2026-11-09"), 16)
        self.assertEqual(slots.generate_day(6, "2026-11-09"), 0)
        self.assertEqual(slots.generate_day(6, "2026-11-10", start="14:00", end="16:00", minutes=60), 2)

    def test_no_slots_on_holidays(self):
        self.assertEqual(slots.generate_day(1, "2026-12-08"), 0)
        self.assertEqual(slots.free(1, "2026-12-08"), [])

    def test_count_free(self):
        self.assertEqual(slots.count_free("2026-11-05"), 63)


if __name__ == "__main__":
    unittest.main()
