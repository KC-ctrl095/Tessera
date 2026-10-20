"""The shipment store: an in-memory stand-in for the database, with a small cursor pool."""

import contextlib
import logging

log = logging.getLogger(__name__)

_OPEN_STORES = []


class PoolLeak(RuntimeError):
    pass


class Store:
    def __init__(self, name="shipments", pool_size=8):
        self.name = name
        self.pool_size = pool_size
        self.rows = {}
        self.open_cursors = 0
        self.leaked_by = []
        _OPEN_STORES.append(self)
        log.debug("store %s opened (pool of %d cursors)", name, pool_size)

    @contextlib.contextmanager
    def cursor(self, who="?"):
        self.open_cursors += 1
        log.debug("store %s: cursor %d/%d taken by %s", self.name, self.open_cursors, self.pool_size, who)
        try:
            yield self
        finally:
            self.open_cursors -= 1
            log.debug("store %s: cursor returned by %s", self.name, who)

    def raw_cursor(self, who="?"):
        """A cursor the caller must give back with release(); used by batch jobs."""
        self.open_cursors += 1
        self.leaked_by.append(who)
        log.debug("store %s: raw cursor taken by %s", self.name, who)
        return self

    def release(self, who="?"):
        self.open_cursors -= 1
        if who in self.leaked_by:
            self.leaked_by.remove(who)

    def put(self, key, row):
        self.rows[key] = row
        log.debug("store %s: put %s", self.name, key)

    def get(self, key):
        return self.rows.get(key)

    def close(self):
        """Close the store; returns the number of cursors that were never given back."""
        if self in _OPEN_STORES:
            _OPEN_STORES.remove(self)
        log.debug("store %s closed", self.name)
        return self.open_cursors


def close_all():
    leaks = []
    for s in list(_OPEN_STORES):
        if s.close():
            leaks.append(s)
    for s in leaks:
        try:
            raise PoolLeak(f"{s.open_cursors} cursor(s) still open in store {s.name!r}")
        except PoolLeak:
            log.exception("connection pool closed with cursors still open (taken by: %s)", ", ".join(s.leaked_by) or "?")
