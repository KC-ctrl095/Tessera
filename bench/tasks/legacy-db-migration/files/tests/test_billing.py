import unittest

from clinic import app, appointments, billing, slots


class BillingTest(unittest.TestCase):
    def setUp(self):
        app.setup()

    def completed_appointment(self, patient_id, practitioner_id, day):
        appt = appointments.book(patient_id, slots.free(practitioner_id, day)[0]["id"])
        appointments.set_outcome(appt, "completed")
        return appt

    def test_invoice_and_payments(self):
        appt = self.completed_appointment(10, 4, "2026-11-03")  # dermatology, no insurer
        inv = billing.invoice_for(appt)
        self.assertEqual(billing.invoice_for(appt), inv)
        self.assertEqual(billing.balance(10), 6500)
        self.assertEqual(billing.record_payment(inv, 2500), 2500)
        self.assertEqual(billing.balance(10), 4000)
        self.assertEqual(billing.record_payment(inv, 4000, "mbway"), 6500)
        self.assertEqual(billing.balance(10), 0)

    def test_insurer_share(self):
        appt = self.completed_appointment(4, 1, "2026-11-03")  # general, Multicare covers 80%
        billing.invoice_for(appt)
        self.assertEqual(billing.balance(4), 400 + 900)
        self.assertEqual(billing.patient_share("Medis", 4500), 1350)


if __name__ == "__main__":
    unittest.main()
