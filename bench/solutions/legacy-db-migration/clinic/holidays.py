"""Days the clinic is closed."""

import db


def is_holiday(day):
    return db.repo().one_or_none("SELECT name FROM holidays WHERE day = ?", (day,)) is not None


def upcoming(from_day, limit=5):
    rows = db.repo().all("SELECT day, name FROM holidays WHERE day >= ? ORDER BY day LIMIT ?", (from_day, limit))
    return [f"{r.day} {r.name}" for r in rows]
