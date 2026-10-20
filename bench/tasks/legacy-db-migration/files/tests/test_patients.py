import unittest

from clinic import app, patients, search
from clinic.errors import Invalid, NotFound


class PatientsTest(unittest.TestCase):
    def setUp(self):
        app.setup()

    def test_create_and_get(self):
        pid = patients.create("Sara Galvão", "1999-02-14", "936 111 222", "sara.g@mail.example")
        p = patients.get(pid)
        self.assertEqual(
            p,
            {
                "id": pid,
                "name": "Sara Galvão",
                "birth_date": "1999-02-14",
                "phone": "+351936111222",
                "email": "sara.g@mail.example",
                "insurer": None,
                "active": True,
            },
        )

    def test_get_unknown(self):
        with self.assertRaises(NotFound):
            patients.get(999)

    def test_invalid(self):
        with self.assertRaises(Invalid):
            patients.create("  ")
        with self.assertRaises(Invalid):
            patients.create("Rui Lemos", phone="12")

    def test_update_contact(self):
        patients.update_contact(4, email="pedro.gaspar@mail.example")
        p = patients.get(4)
        self.assertEqual((p["phone"], p["email"]), ("+351936220418", "pedro.gaspar@mail.example"))

    def test_deactivate(self):
        self.assertTrue(patients.deactivate(5))
        self.assertFalse(patients.deactivate(5))
        self.assertFalse(patients.get(5)["active"])
        self.assertEqual([r["id"] for r in search.by_name("leonor")], [])

    def test_search_by_name(self):
        self.assertEqual(
            search.by_name("MATOS"),
            [
                {"id": 1, "name": "Joana Matos", "phone": "+351912400101"},
                {"id": 13, "name": "Joana Matos", "phone": "+351912400101"},
                {"id": 2, "name": "Tiago Matos", "phone": "+351912400101"},
            ],
        )
        self.assertEqual(len(search.by_name("a", limit=4)), 4)


if __name__ == "__main__":
    unittest.main()
