"""db.Repo: queries return Row objects, writes return counts or ids, and
transactions are `with repo.transaction():` blocks. See MIGRATION.md.

This module is finished and reviewed (tests/test_repo.py). Don't change it.
"""

import contextlib
import sqlite3

_TRANSACTION_WORDS = {"BEGIN", "COMMIT", "END", "ROLLBACK", "SAVEPOINT", "RELEASE"}


class NoRow(LookupError):
    """one() found nothing."""


class TooManyRows(LookupError):
    """one() or one_or_none() found more than one row."""


class AmbiguousColumns(ValueError):
    """A result has two columns with the same name (SELECT * over a join, p.name and pr.name...)."""


class UseTransaction(ValueError):
    """BEGIN/COMMIT/ROLLBACK passed as SQL; use `with repo.transaction():`."""


class Row:
    """One result row. Read columns by name: row.name or row["name"].

    Rows are not tuples: row[0], unpacking (a, b = row) and iterating raise
    TypeError, so code can't silently depend on the order of the SELECT.
    """

    __slots__ = ("_values",)

    def __init__(self, names, values):
        object.__setattr__(self, "_values", dict(zip(names, values)))

    def __getattr__(self, name):
        try:
            return self._values[name]
        except KeyError:
            raise AttributeError(f"no column {name!r} in this row (it has {', '.join(self._values)})") from None

    def __getitem__(self, key):
        if not isinstance(key, str):
            raise TypeError("a Row is not a tuple: use the column name (row.name or row['name'])")
        return self._values[key]

    def __iter__(self):
        raise TypeError("a Row is not a tuple: use the column names (row.name), or row.as_dict()")

    def __setattr__(self, name, value):
        raise AttributeError("rows are read-only")

    def keys(self):
        return list(self._values)

    def get(self, name, default=None):
        return self._values.get(name, default)

    def as_dict(self) -> dict:
        return dict(self._values)

    def __eq__(self, other):
        return isinstance(other, Row) and self._values == other._values

    __hash__ = None

    def __repr__(self):
        return "Row(" + ", ".join(f"{k}={v!r}" for k, v in self._values.items()) + ")"


class Repo:
    def __init__(self, conn: sqlite3.Connection):
        self._conn = conn
        self._depth = 0

    @classmethod
    def connect(cls, path=":memory:") -> "Repo":
        conn = sqlite3.connect(path, isolation_level=None)
        conn.execute("PRAGMA foreign_keys = ON")
        return cls(conn)

    def close(self) -> None:
        self._conn.close()

    # -- queries ---------------------------------------------------------------

    def all(self, sql: str, params=()) -> list:
        """Every row of a query, as Row objects."""
        return self._rows(self._run(sql, params))

    def one(self, sql: str, params=()) -> Row:
        """The only row of a query: NoRow when there is none, TooManyRows when there are several."""
        rows = self.all(sql, params)
        if not rows:
            raise NoRow(f"no row for: {sql}")
        if len(rows) > 1:
            raise TooManyRows(f"{len(rows)} rows for: {sql}")
        return rows[0]

    def one_or_none(self, sql: str, params=()):
        """The only row of a query, or None when there is none; TooManyRows when there are several."""
        rows = self.all(sql, params)
        if len(rows) > 1:
            raise TooManyRows(f"{len(rows)} rows for: {sql}")
        return rows[0] if rows else None

    def scalar(self, sql: str, params=()):
        """The value of a query that returns one row with one column (COUNT, SUM, MAX...)."""
        row = self.one(sql, params)
        values = row.as_dict()
        if len(values) != 1:
            raise ValueError(f"scalar() needs exactly one column, got {', '.join(values)}")
        return next(iter(values.values()))

    # -- writes ----------------------------------------------------------------

    def execute(self, sql: str, params=()) -> int:
        """Runs an UPDATE/DELETE/INSERT; returns the number of rows it changed."""
        return self._run(sql, params).rowcount

    def insert(self, sql: str, params=()) -> int:
        """Runs an INSERT; returns the id of the new row."""
        return self._run(sql, params).lastrowid

    def script(self, sql: str) -> None:
        """Runs several statements (schema setup only; it commits on its own)."""
        if self._depth:
            raise UseTransaction("script() can't run inside a transaction")
        self._conn.executescript(sql)

    # -- transactions ----------------------------------------------------------

    @contextlib.contextmanager
    def transaction(self):
        """Everything in the block commits together, or not at all if it raises.

        Blocks can be nested (the inner one becomes a savepoint).
        """
        name = f"repo_sp{self._depth}"
        self._conn.execute("BEGIN" if self._depth == 0 else f"SAVEPOINT {name}")
        self._depth += 1
        try:
            yield self
        except BaseException:
            self._depth -= 1
            if self._depth == 0:
                self._conn.execute("ROLLBACK")
            else:
                self._conn.execute(f"ROLLBACK TO {name}")
                self._conn.execute(f"RELEASE {name}")
            raise
        self._depth -= 1
        self._conn.execute("COMMIT" if self._depth == 0 else f"RELEASE {name}")

    @property
    def in_transaction(self) -> bool:
        return self._depth > 0

    # -- internals -------------------------------------------------------------

    def _run(self, sql, params):
        if isinstance(params, (str, bytes)) or not isinstance(params, (tuple, list, dict)):
            raise TypeError(
                f"params must be a tuple, list or dict, not {type(params).__name__} (one value is written (value,))"
            )
        words = sql.split(None, 1)
        if words and words[0].upper().rstrip(";") in _TRANSACTION_WORDS:
            raise UseTransaction(f"{words[0]} as SQL: use `with repo.transaction():`")
        return self._conn.execute(sql, params)

    @staticmethod
    def _rows(cur) -> list:
        if cur.description is None:
            return []
        names = [d[0] for d in cur.description]
        dupes = sorted({n for n in names if names.count(n) > 1})
        if dupes:
            raise AmbiguousColumns(
                f"several columns named {', '.join(dupes)}: list the columns and give them distinct names (AS ...)"
            )
        return [Row(names, values) for values in cur.fetchall()]
