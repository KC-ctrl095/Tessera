"""Reading books from the database."""

from .models import Book


class BookRepository:
    def __init__(self, conn):
        self.conn = conn

    def list_books(self, author=None):
        """Books ordered by title, then id; only one author's when `author` is given."""
        rows = self.conn.execute(
            "SELECT id, title, author, year FROM books"
            " WHERE (? IS NULL OR author = ?)"
            " ORDER BY title, id",
            (author, author),
        ).fetchall()
        return [Book(*row) for row in rows]

    def get(self, book_id):
        row = self.conn.execute("SELECT id, title, author, year FROM books WHERE id = ?", (book_id,)).fetchone()
        return Book(*row) if row else None
