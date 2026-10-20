import importlib
import subprocess
import sys
import unittest
import warnings


class NewNamesTest(unittest.TestCase):
    def test_new_names_everywhere(self):
        import shopkit
        from shopkit import UserRecord, get_user
        from shopkit.users import UserRecord as U2, get_user as g2

        self.assertIs(UserRecord, U2)
        self.assertIs(get_user, g2)
        u = get_user(2)
        self.assertIsInstance(u, UserRecord)
        self.assertEqual(type(u).__name__, "UserRecord")
        self.assertIn("get_user", shopkit.__all__)
        self.assertIn("UserRecord", shopkit.__all__)

    def test_new_name_does_not_warn(self):
        from shopkit import get_user

        with warnings.catch_warnings():
            warnings.simplefilter("error")
            get_user(1)

    def test_orders_use_the_new_class(self):
        from shopkit import UserRecord, orders_for

        self.assertIsInstance(orders_for(2)[0].customer, UserRecord)

    def test_star_import(self):
        ns = {}
        exec("from shopkit import *", ns)
        self.assertIn("get_user", ns)
        self.assertIn("UserRecord", ns)


class OldNamesTest(unittest.TestCase):
    def test_get_usr_still_works_and_warns(self):
        import shopkit

        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            u = shopkit.get_usr(3)
        self.assertEqual(u.name, "Mira Okafor")
        self.assertTrue(any(issubclass(w.category, DeprecationWarning) for w in caught), caught)

    def test_get_usr_keeps_its_errors(self):
        import shopkit

        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            with self.assertRaises(KeyError):
                shopkit.get_usr(99)

    def test_old_class_name(self):
        import shopkit

        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            old = shopkit.UsrRecord
            u = shopkit.get_user(1)
            self.assertIsInstance(u, old)
            made = old(id=7, name="Lee", email="lee@example.org")
        self.assertEqual(made.display(), "Lee <lee@example.org>")


class CliTest(unittest.TestCase):
    def test_cli(self):
        out = subprocess.run([sys.executable, "-m", "shopkit.cli", "1"], capture_output=True, text=True)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("Ada Brandt", out.stdout)
        self.assertNotIn("DeprecationWarning", out.stderr)
