"""Bookable time slots of each practitioner."""

import db

from . import audit, holidays, timeutil, validation


def generate_day(practitioner_id, day, start="09:00", end="17:00", minutes=30):
    """Creates the day's slots (none on holidays); returns how many were new."""
    validation.require_day(day)
    if holidays.is_holiday(day):
        return 0
    repo = db.repo()
    with repo.transaction():
        created = 0
        for t in timeutil.times_between(start, end, minutes):
            created += repo.execute(
                "INSERT OR IGNORE INTO slots (practitioner_id, starts_at, minutes) VALUES (?, ?, ?)",
                (practitioner_id, f"{day}T{t}", minutes),
            )
        audit.log("slots.generated", practitioner_id, f"{day}: {created}")
    return created


def free(practitioner_id, day):
    rows = db.repo().all(
        "SELECT id, starts_at, minutes FROM slots"
        " WHERE practitioner_id = ? AND taken = 0 AND starts_at LIKE ? ORDER BY starts_at",
        (practitioner_id, day + "%"),
    )
    return [{"id": r.id, "starts_at": r.starts_at, "minutes": r.minutes} for r in rows]


def next_free(practitioner_id, after):
    """The practitioner's first free slot at or after `after`, or None."""
    row = db.repo().one_or_none(
        "SELECT id, starts_at FROM slots WHERE practitioner_id = ? AND taken = 0 AND starts_at >= ?"
        " ORDER BY starts_at LIMIT 1",
        (practitioner_id, after),
    )
    if row is None:
        return None
    return {"id": row.id, "starts_at": row.starts_at}


def count_free(day):
    return db.repo().scalar("SELECT COUNT(*) FROM slots WHERE taken = 0 AND substr(starts_at, 1, 10) = ?", (day,))
