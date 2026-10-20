"""What the API does, independent of HTTP."""

from dataclasses import asdict


def list_books(repo, author=None):
    """Every book (or one author's), as dicts, ordered by title then id."""
    return [asdict(b) for b in repo.list_books(author=author)]


def get_book(repo, book_id):
    book = repo.get(book_id)
    return asdict(book) if book else None
