"""The database layer. `db.connect(path)` opens the shared Repo, `db.repo()` returns it."""

from sqlite3 import IntegrityError

from .repo import AmbiguousColumns, NoRow, Repo, Row, TooManyRows, UseTransaction

__all__ = [
    "AmbiguousColumns",
    "IntegrityError",
    "NoRow",
    "Repo",
    "Row",
    "TooManyRows",
    "UseTransaction",
    "connect",
    "repo",
]

_repo = None


def connect(path=":memory:") -> Repo:
    """Opens the database at `path` as the shared Repo (closing the previous one)."""
    global _repo
    if _repo is not None:
        _repo.close()
    _repo = Repo.connect(path)
    return _repo


def repo() -> Repo:
    """The shared Repo opened by connect()."""
    if _repo is None:
        raise RuntimeError("db.connect() wasn't called")
    return _repo
