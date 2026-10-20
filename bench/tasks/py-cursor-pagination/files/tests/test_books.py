import unittest

from app.db import connect
from app.handlers import get_book, get_books
from app.repository import BookRepository


class BooksTest(unittest.TestCase):
    def setUp(self):
        self.repo = BookRepository(connect())

    def test_order(self):
        status, body = get_books(self.repo, {})
        self.assertEqual(status, 200)
        titles = [b["title"] for b in body["books"]]
        self.assertEqual(titles[:4], ["A Map of Salt", "Atlas of Small Things", "Bridges", "Collected Poems"])

    def test_author_filter(self):
        status, body = get_books(self.repo, {"author": "Lena Ford"})
        self.assertEqual([b["title"] for b in body["books"]], ["Bridges", "Ember", "Keepsake", "Quiet Hours"])

    def test_get_book(self):
        self.assertEqual(get_book(self.repo, 1)[1]["title"], "The Long Harbour")
        self.assertEqual(get_book(self.repo, 999)[0], 404)
