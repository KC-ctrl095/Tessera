"""Durations as people type them in our job scheduler's config ("1h30m", "90s")."""


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
    raise NotImplementedError
