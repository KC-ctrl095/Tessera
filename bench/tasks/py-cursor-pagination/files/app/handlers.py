"""HTTP handlers. `query` holds the URL's query parameters as strings; each
handler returns (status code, JSON body)."""

from . import service


def get_books(repo, query):
    """GET /books[?author=NAME]"""
    author = query.get("author") or None
    return 200, {"books": service.list_books(repo, author=author)}


def get_book(repo, book_id):
    """GET /books/<id>"""
    book = service.get_book(repo, book_id)
    if book is None:
        return 404, {"error": "no such book"}
    return 200, book
