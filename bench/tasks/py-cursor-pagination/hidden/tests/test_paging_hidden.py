import unittest

from app.cursor import encode_cursor
from app.db import SAMPLE, connect
from app.handlers import get_book, get_books
from app.repository import BookRepository


def all_sorted(rows, author=None):
    with_ids = [(t, i + 1, a) for i, (t, a, _y) in enumerate(rows) if author is None or a == author]
    return [(t, i) for t, i, _a in sorted(with_ids)]


class PagingTest(unittest.TestCase):
    def setUp(self):
        self.conn = connect()
        self.repo = BookRepository(self.conn)

    def page(self, **q):
        status, body = get_books(self.repo, {k: str(v) for k, v in q.items()})
        self.assertEqual(status, 200, body)
        return body

    def walk(self, limit, author=None):
        seen, cursor, pages = [], None, 0
        while True:
            q = {"limit": limit}
            if author:
                q["author"] = author
            if cursor:
                q["after"] = cursor
            body = self.page(**q)
            self.assertLessEqual(len(body["books"]), limit)
            seen += [(b["title"], b["id"]) for b in body["books"]]
            pages += 1
            cursor = body["next_cursor"]
            if cursor is None:
                return seen, pages
            self.assertLess(pages, 50)

    def test_default_limit_is_20(self):
        body = self.page()
        self.assertEqual(len(body["books"]), 20)
        self.assertIsInstance(body["next_cursor"], str)
        self.assertEqual(set(body["books"][0]), {"id", "title", "author", "year"})

    def test_walk_all_pages(self):
        for limit in (1, 3, 5, 7, 13, 26, 100):
            seen, pages = self.walk(limit)
            self.assertEqual(seen, all_sorted(SAMPLE), f"limit {limit}")
            self.assertEqual(pages, -(-len(SAMPLE) // limit), f"limit {limit}: an exactly full last page has next_cursor null")

    def test_duplicate_titles_across_pages(self):
        # Three books are titled "Collected Poems": every page size must see each once.
        for limit in (2, 4):
            seen, _ = self.walk(limit)
            self.assertEqual([i for t, i in seen if t == "Collected Poems"], [2, 4, 9])

    def test_author_with_paging(self):
        seen, pages = self.walk(2, author="Ines Duarte")
        self.assertEqual(seen, all_sorted(SAMPLE, "Ines Duarte"))
        self.assertEqual(pages, 2)

    def test_stable_when_books_are_added(self):
        first = self.page(limit=5)
        last = first["books"][-1]
        self.conn.execute("INSERT INTO books (title, author, year) VALUES ('Aardvark Days', 'New Author', 2024)")
        second = self.page(limit=5, after=first["next_cursor"])
        rest = [(t, i) for t, i in all_sorted(SAMPLE) if (t, i) > (last["title"], last["id"])][:5]
        self.assertEqual([(b["title"], b["id"]) for b in second["books"]], rest)

    def test_cursor_from_the_helpers(self):
        body = self.page(limit=2, after=encode_cursor("Collected Poems", 2))
        self.assertEqual([(b["title"], b["id"]) for b in body["books"]], [("Collected Poems", 4), ("Collected Poems", 9)])

    def test_bad_limits(self):
        for bad in ("0", "101", "-1", "abc", "5.5", "", " 7x"):
            status, body = get_books(self.repo, {"limit": bad})
            self.assertEqual((status, body), (400, {"error": "limit must be an integer from 1 to 100"}), repr(bad))
        self.assertEqual(len(self.page(limit=100)["books"]), 26)
        self.assertEqual(len(self.page(limit=1)["books"]), 1)

    def test_bad_cursor(self):
        for bad in ("nonsense", "e30", encode_cursor("x", 1)[:-3] + "!!!"):
            status, body = get_books(self.repo, {"after": bad})
            self.assertEqual((status, body), (400, {"error": "invalid cursor"}), repr(bad))

    def test_limit_happens_in_sql(self):
        statements = []
        self.conn.set_trace_callback(statements.append)
        body = self.page(limit=3, after=encode_cursor("Bridges", 6))
        self.conn.set_trace_callback(None)
        self.assertEqual(len(body["books"]), 3)
        selects = [s for s in statements if s.lstrip().upper().startswith("SELECT")]
        self.assertTrue(selects, statements)
        self.assertTrue(all("LIMIT" in s.upper() for s in selects), selects)

    def test_get_book_still_works(self):
        self.assertEqual(get_book(self.repo, 6)[1]["title"], "Bridges")


if __name__ == "__main__":
    unittest.main()
