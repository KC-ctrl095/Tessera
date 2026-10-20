"""Invoice numbers: FT <year>/<sequence>, one sequence per year."""

_counters = {}


def next_number(year: int) -> str:
    _counters[year] = _counters.get(year, 0) + 1
    return f"FT {year}/{_counters[year]:05d}"


def reset():
    _counters.clear()
