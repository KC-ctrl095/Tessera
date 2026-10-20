"""Bookable time slots of each practitioner."""

import legacy_db

from . import audit, holidays, timeutil, validation
from .errors import NotFound


def generate_day(practitioner_id, day, start="09:00", end="17:00", minutes=30):
    """Creates the day's slots (none on holidays); returns how many were new."""
    validation.require_day(day)
    if holidays.is_holiday(day):
        return 0
    legacy_db.run("BEGIN", fetch=None)
    try:
        created = 0
        for t in timeutil.times_between(start, end, minutes):
            created += legacy_db.run(
                "INSERT OR IGNORE INTO slots (practitioner_id, starts_at, minutes) VALUES (?, ?, ?)",
                practitioner_id,
                f"{day}T{t}",
                minutes,
                fetch=None,
            )
        audit.log("slots.generated", practitioner_id, f"{day}: {created}")
        legacy_db.run("COMMIT", fetch=None)
    except Exception:
        legacy_db.run("ROLLBACK", fetch=None)
        raise
    return created


def free(practitioner_id, day):
    rows = legacy_db.run(
        "SELECT id, starts_at, minutes FROM slots"
        " WHERE practitioner_id = ? AND taken = 0 AND starts_at LIKE ? ORDER BY starts_at",
        practitioner_id,
        day + "%",
    )
    return [{"id": i, "starts_at": s, "minutes": m} for i, s, m in rows]


def next_free(practitioner_id, after):
    """The practitioner's first free slot at or after `after`, or None."""
    row = legacy_db.run(
        "SELECT id, starts_at FROM slots WHERE practitioner_id = ? AND taken = 0 AND starts_at >= ? ORDER BY starts_at",
        practitioner_id,
        after,
        fetch="one",
    )
    if row is None:
        return None
    return {"id": row[0], "starts_at": row[1]}


def count_free(day):
    return legacy_db.run(
        "SELECT COUNT(*) FROM slots WHERE taken = 0 AND substr(starts_at, 1, 10) = ?", day, fetch="one"
    )[0]
