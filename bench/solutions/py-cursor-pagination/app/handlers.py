"""HTTP handlers. `query` holds the URL's query parameters as strings; each
handler returns (status code, JSON body)."""

from . import service
from .cursor import decode_cursor

LIMIT_ERROR = {"error": "limit must be an integer from 1 to 100"}


def get_books(repo, query):
    """GET /books[?author=NAME][&limit=N][&after=CURSOR]"""
    author = query.get("author") or None
    raw = query.get("limit")
    limit = 20
    if raw is not None:
        if not raw.isdigit() or not 1 <= int(raw) <= 100:
            return 400, LIMIT_ERROR
        limit = int(raw)
    after = None
    if query.get("after") is not None:
        try:
            after = decode_cursor(query["after"])
        except ValueError:
            return 400, {"error": "invalid cursor"}
    books, next_cursor = service.list_books(repo, author=author, after=after, limit=limit)
    return 200, {"books": books, "next_cursor": next_cursor}


def get_book(repo, book_id):
    """GET /books/<id>"""
    book = service.get_book(repo, book_id)
    if book is None:
        return 404, {"error": "no such book"}
    return 200, book
