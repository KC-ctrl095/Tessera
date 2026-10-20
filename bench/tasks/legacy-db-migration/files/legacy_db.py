"""The old database helper. It is being replaced by db.Repo: see MIGRATION.md."""

import sqlite3

_conn = None


def connect(path=":memory:"):
    """Opens the database in autocommit mode; transactions are BEGIN/COMMIT statements."""
    global _conn
    if _conn is not None:
        _conn.close()
    _conn = sqlite3.connect(path, isolation_level=None)
    _conn.execute("PRAGMA foreign_keys = ON")
    return _conn


def connection():
    if _conn is None:
        raise RuntimeError("legacy_db.connect() wasn't called")
    return _conn


def run(sql, *args, fetch="all"):
    """Runs one statement with `args` as its parameters.

    fetch="all": every row, as a list of tuples
    fetch="one": the first row (a tuple), or None when there is none
    fetch="id":  the id of the row an INSERT created
    fetch=None:  the number of rows the statement changed
    """
    cur = connection().execute(sql, args)
    if fetch == "all":
        return cur.fetchall()
    if fetch == "one":
        return cur.fetchone()
    if fetch == "id":
        return cur.lastrowid
    if fetch is None:
        return cur.rowcount
    raise ValueError(f"unknown fetch mode {fetch!r}")
