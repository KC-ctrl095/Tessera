"""Days the clinic is closed."""

import legacy_db


def is_holiday(day):
    return legacy_db.run("SELECT name FROM holidays WHERE day = ?", day, fetch="one") is not None


def upcoming(from_day, limit=5):
    rows = legacy_db.run("SELECT day, name FROM holidays WHERE day >= ? ORDER BY day LIMIT ?", from_day, limit)
    return [f"{d} {n}" for d, n in rows]
