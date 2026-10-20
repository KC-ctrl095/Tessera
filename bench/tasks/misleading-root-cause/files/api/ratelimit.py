"""Bursts: at most PER_MINUTE calls in any rolling minute (on top of the daily quota)."""

from collections import defaultdict, deque
from datetime import timedelta

PER_MINUTE = 60
WINDOW = timedelta(minutes=1)


class Burst:
    def __init__(self, per_minute: int = PER_MINUTE):
        self.per_minute = per_minute
        self._recent = defaultdict(deque)

    def allow(self, key, now) -> bool:
        q = self._recent[key]
        while q and q[0] <= now - WINDOW:
            q.popleft()
        if len(q) >= self.per_minute:
            return False
        q.append(now)
        return True
