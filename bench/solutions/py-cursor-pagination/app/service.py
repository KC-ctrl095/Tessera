"""What the API does, independent of HTTP."""

from dataclasses import asdict

from .cursor import encode_cursor


def list_books(repo, author=None, after=None, limit=20):
    """One page of books (dicts, by title then id) and the cursor of the next page (or None)."""
    rows = repo.list_books(author=author, after=after, limit=limit + 1)
    page = rows[:limit]
    next_cursor = encode_cursor(page[-1].title, page[-1].id) if len(rows) > limit else None
    return [asdict(b) for b in page], next_cursor


def get_book(repo, book_id):
    book = repo.get(book_id)
    return asdict(book) if book else None
