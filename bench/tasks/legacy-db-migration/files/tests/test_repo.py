import unittest

import db
from db import AmbiguousColumns, NoRow, Repo, TooManyRows, UseTransaction


class RepoTest(unittest.TestCase):
    def setUp(self):
        self.repo = Repo.connect()
        self.repo.script(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT UNIQUE, n INTEGER);"
            "CREATE TABLE u (id INTEGER PRIMARY KEY, name TEXT);"
        )
        for name, n in (("a", 1), ("b", 2), ("c", 2)):
            self.repo.insert("INSERT INTO t (name, n) VALUES (?, ?)", (name, n))

    def test_rows(self):
        rows = self.repo.all("SELECT id, name FROM t ORDER BY id")
        self.assertEqual([(r.id, r.name) for r in rows], [(1, "a"), (2, "b"), (3, "c")])
        self.assertEqual(rows[0]["name"], "a")
        self.assertEqual(rows[0].as_dict(), {"id": 1, "name": "a"})
        self.assertEqual(rows[0].keys(), ["id", "name"])

    def test_rows_are_not_tuples(self):
        row = self.repo.one("SELECT id, name FROM t WHERE id = ?", (1,))
        with self.assertRaises(TypeError):
            row[0]
        with self.assertRaises(TypeError):
            a, b = row
        with self.assertRaises(TypeError):
            list(row)
        with self.assertRaises(AttributeError):
            row.missing

    def test_one(self):
        self.assertEqual(self.repo.one("SELECT name FROM t WHERE id = ?", (2,)).name, "b")
        with self.assertRaises(NoRow):
            self.repo.one("SELECT name FROM t WHERE id = ?", (9,))
        with self.assertRaises(TooManyRows):
            self.repo.one("SELECT name FROM t WHERE n = ?", (2,))
        self.assertEqual(self.repo.one("SELECT name FROM t WHERE n = ? ORDER BY name DESC LIMIT 1", (2,)).name, "c")

    def test_one_or_none(self):
        self.assertIsNone(self.repo.one_or_none("SELECT name FROM t WHERE id = ?", (9,)))
        self.assertEqual(self.repo.one_or_none("SELECT name FROM t WHERE id = ?", (1,)).name, "a")
        with self.assertRaises(TooManyRows):
            self.repo.one_or_none("SELECT name FROM t WHERE n = 2")

    def test_scalar_execute_insert(self):
        self.assertEqual(self.repo.scalar("SELECT COUNT(*) FROM t"), 3)
        self.assertEqual(self.repo.execute("UPDATE t SET n = 5 WHERE n = ?", (2,)), 2)
        self.assertEqual(self.repo.insert("INSERT INTO t (name, n) VALUES (?, ?)", ("d", 4)), 4)
        self.assertEqual(self.repo.scalar("SELECT COUNT(*) AS n FROM t WHERE name = :name", {"name": "d"}), 1)

    def test_params_must_be_a_sequence(self):
        with self.assertRaises(TypeError):
            self.repo.all("SELECT name FROM t WHERE id = ?", 1)
        with self.assertRaises(TypeError):
            self.repo.all("SELECT name FROM t WHERE name = ?", "a")

    def test_duplicate_names(self):
        self.repo.insert("INSERT INTO u (name) VALUES (?)", ("x",))
        with self.assertRaises(AmbiguousColumns):
            self.repo.all("SELECT * FROM t JOIN u ON u.id = t.id")
        rows = self.repo.all("SELECT t.name AS t_name, u.name AS u_name FROM t JOIN u ON u.id = t.id")
        self.assertEqual((rows[0].t_name, rows[0].u_name), ("a", "x"))

    def test_transaction_commits(self):
        with self.repo.transaction():
            self.repo.execute("UPDATE t SET n = 0")
            self.repo.insert("INSERT INTO t (name, n) VALUES ('e', 0)", ())
        self.assertEqual(self.repo.scalar("SELECT SUM(n) FROM t"), 0)
        self.assertEqual(self.repo.scalar("SELECT COUNT(*) FROM t"), 4)

    def test_transaction_rolls_back(self):
        with self.assertRaises(db.IntegrityError):
            with self.repo.transaction():
                self.repo.execute("UPDATE t SET n = 0")
                self.repo.insert("INSERT INTO t (name, n) VALUES ('a', 0)")  # duplicate name
        self.assertEqual(self.repo.scalar("SELECT SUM(n) FROM t"), 5)
        self.assertFalse(self.repo.in_transaction)

    def test_nested_transactions(self):
        with self.repo.transaction():
            self.repo.execute("UPDATE t SET n = 10 WHERE id = 1")
            with self.assertRaises(ValueError):
                with self.repo.transaction():
                    self.repo.execute("UPDATE t SET n = 20 WHERE id = 2")
                    raise ValueError("inner fails")
        self.assertEqual([r.n for r in self.repo.all("SELECT n FROM t ORDER BY id")], [10, 2, 2])

    def test_no_raw_transactions(self):
        for sql in ("BEGIN", "commit", "ROLLBACK", "  begin transaction"):
            with self.assertRaises(UseTransaction):
                self.repo.execute(sql)


class SharedRepoTest(unittest.TestCase):
    def test_connect_and_repo(self):
        r = db.connect()
        self.assertIs(db.repo(), r)
        r2 = db.connect()
        self.assertIs(db.repo(), r2)


if __name__ == "__main__":
    unittest.main()
