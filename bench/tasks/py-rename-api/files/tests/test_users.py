import unittest

from shopkit import UsrRecord, get_usr, list_users


class UsersTest(unittest.TestCase):
    def test_get_usr(self):
        u = get_usr(1)
        self.assertIsInstance(u, UsrRecord)
        self.assertEqual(u.name, "Ada Brandt")

    def test_missing(self):
        with self.assertRaises(KeyError):
            get_usr(99)

    def test_staff_filter(self):
        self.assertEqual([u.id for u in list_users(staff=True)], [3])
