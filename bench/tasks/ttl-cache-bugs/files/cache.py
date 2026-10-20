"""A small in-process cache with a size limit and expiry (in front of the pricing API)."""

import time
from collections import OrderedDict


class TTLCache:
    """Holds up to `maxsize` entries; each expires `ttl` seconds after it was set.

    - get(key, default=None): the value, or `default` when the key is missing
      or expired. Any stored value counts, also 0, "" or None. A successful get
      counts as a use of the entry (see eviction).
    - set(key, value): store or replace. Replacing restarts the entry's ttl
      and counts as a use.
    - delete(key): remove the key; returns True if a live entry was removed
      (an expired one counts as absent, and is dropped too).
    - len(cache) and `key in cache` count only entries that haven't expired.
    - Eviction: setting a new key into a full cache first drops every expired
      entry; if the cache is still full, the least recently used entry goes.
      Only get and set count as uses (`in` and len() don't).
    - `clock` returns the current time in seconds (time.monotonic by
      default), so tests can control time. An entry set at time t lives while
      clock() < t + ttl.
    """

    def __init__(self, maxsize=128, ttl=60.0, clock=time.monotonic):
        if maxsize < 1:
            raise ValueError("maxsize must be at least 1")
        self.maxsize = maxsize
        self.ttl = ttl
        self.clock = clock
        # key -> (expires_at, value), least recently used first
        self._data = OrderedDict()

    def _live(self, key, now):
        entry = self._data.get(key)
        if entry is None:
            return None
        if now > entry[0]:
            del self._data[key]
            return None
        return entry

    def get(self, key, default=None):
        entry = self._live(key, self.clock())
        if entry is None:
            return default
        return entry[1] or default

    def set(self, key, value):
        now = self.clock()
        if key in self._data:
            self._data[key] = (self._data[key][0], value)
            return
        if len(self._data) >= self.maxsize:
            self._data.popitem(last=False)
        self._data[key] = (now + self.ttl, value)

    def delete(self, key):
        live = self._live(key, self.clock()) is not None
        if live:
            del self._data[key]
        return live

    def _purge(self, now):
        for key in [k for k, (expires, _) in self._data.items() if now >= expires]:
            del self._data[key]

    def __len__(self):
        return len(self._data)

    def __contains__(self, key):
        return self._live(key, self.clock()) is not None
