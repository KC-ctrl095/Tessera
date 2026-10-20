import unittest

from dedupe import find_duplicates


class FindDuplicatesTest(unittest.TestCase):
    def test_email_groups(self):
        a = {"id": 1, "email": "Ana.Silva@gmail.com"}
        b = {"id": 2, "email": "bob@example.org"}
        c = {"id": 3, "email": "anasilva+news@gmail.com"}
        d = {"id": 4, "email": " BOB@example.org "}
        groups = find_duplicates([a, b, c, d])
        self.assertEqual(groups, [[a, c], [b, d]])
        self.assertIs(groups[0][0], a)

    def test_no_duplicates(self):
        self.assertEqual(find_duplicates([{"email": "x@y.z"}, {"email": "q@y.z"}]), [])

    def test_two_fields(self):
        a = {"name": "Lee  Chan", "city": "Porto"}
        b = {"name": "lee chan", "city": "Porto "}
        c = {"name": "Lee Chan", "city": "Braga"}
        self.assertEqual(find_duplicates([a, b, c], key_fields=("name", "city")), [[a, b]])


if __name__ == "__main__":
    unittest.main()
