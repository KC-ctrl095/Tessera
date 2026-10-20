"""Reading books from the database."""

from .models import Book


class BookRepository:
    def __init__(self, conn):
        self.conn = conn

    def list_books(self, author=None, after=None, limit=None):
        """Books ordered by title, then id; only one author's when `author` is given.

        `after` is a (title, id) position: only books after it. `limit` caps the rows.
        """
        sql = "SELECT id, title, author, year FROM books WHERE (? IS NULL OR author = ?)"
        params = [author, author]
        if after is not None:
            sql += " AND (title > ? OR (title = ? AND id > ?))"
            params += [after[0], after[0], after[1]]
        sql += " ORDER BY title, id"
        if limit is not None:
            sql += " LIMIT ?"
            params.append(limit)
        return [Book(*row) for row in self.conn.execute(sql, params).fetchall()]

    def get(self, book_id):
        row = self.conn.execute("SELECT id, title, author, year FROM books WHERE id = ? LIMIT 1", (book_id,)).fetchone()
        return Book(*row) if row else None
