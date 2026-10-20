"""Durations as people type them in our job scheduler's config ("1h30m", "90s")."""

import re

_UNITS = {"d": 86400.0, "h": 3600.0, "m": 60.0, "s": 1.0, "ms": 0.001}
_ORDER = ["d", "h", "m", "s", "ms"]
_COMPONENT = re.compile(r"([0-9]+(?:\.[0-9]+)?)(ms|[dhms])", re.ASCII | re.IGNORECASE)


def parse_duration(text: str) -> float:
    """Parse a duration like "1h30m" and return it in seconds (a float).

    Rules:
    - A duration is one or more components. A component is a number directly
      followed by a unit, with no space in between: "90s", "1.5h".
    - Units: d (days), h (hours), m (minutes), s (seconds), ms (milliseconds).
      Units are case-insensitive ("1H30M" is fine).
    - Numbers are whole ("90") or decimal with digits on both sides of the
      point ("1.5", "0.25"). "1." and ".5" are invalid.
    - Components may be separated by spaces ("1h 30m"), but don't have to be.
      Leading and trailing spaces are ignored.
    - Units must go from largest to smallest and each unit may appear only
      once: "1h30m" is fine, "30m1h" and "1m1m" are invalid.
    - A single "-" at the very start makes the whole duration negative:
      "-1h30m" is -5400. A sign anywhere else is invalid.
    - A bare number without a unit is invalid, except "0", which means 0.
    - Anything else (empty text, unknown units, stray characters) is invalid.

    Invalid input raises ValueError.
    """
    s = text.strip(" ")
    sign = 1.0
    if s.startswith("-"):
        sign, s = -1.0, s[1:]
    if s == "0" and sign > 0:
        return 0.0
    if not s or s[0] == " ":
        raise ValueError(f"invalid duration: {text!r}")
    pos, last, total = 0, -1, 0.0
    while pos < len(s):
        if s[pos] == " " and pos > 0:
            pos += 1
            continue
        m = _COMPONENT.match(s, pos)
        if not m:
            raise ValueError(f"invalid duration: {text!r}")
        unit = m.group(2).lower()
        rank = _ORDER.index(unit)
        if rank <= last:
            raise ValueError(f"units out of order or repeated in {text!r}")
        last = rank
        total += float(m.group(1)) * _UNITS[unit]
        pos = m.end()
        if pos < len(s) and s[pos] not in " 0123456789":
            raise ValueError(f"invalid duration: {text!r}")
    return sign * total
