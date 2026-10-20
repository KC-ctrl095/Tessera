"""Opaque cursors for paging: they carry the position of the last item shown."""

import base64
import json


def encode_cursor(title, book_id):
    """A cursor pointing just after the book (title, book_id)."""
    raw = json.dumps([title, book_id], separators=(",", ":")).encode()
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def decode_cursor(cursor):
    """(title, book_id) from a cursor; ValueError when it isn't one of ours."""
    try:
        raw = base64.urlsafe_b64decode(cursor + "=" * (-len(cursor) % 4))
        title, book_id = json.loads(raw)
    except Exception as e:  # noqa: BLE001 - any garbage is just an invalid cursor
        raise ValueError("invalid cursor") from e
    if not isinstance(title, str) or not isinstance(book_id, int) or isinstance(book_id, bool):
        raise ValueError("invalid cursor")
    return title, book_id
