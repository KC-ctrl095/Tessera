"""Invoice numbers: TR-<Lisbon year>-<sequence>, the sequence restarting each year."""

from collections import defaultdict

from utils.tz import local_date


class Numbering:
    def __init__(self):
        self._last = defaultdict(int)

    def next(self, issued_at) -> str:
        year = local_date(issued_at).year
        self._last[year] += 1
        return f"TR-{year}-{self._last[year]:05d}"
