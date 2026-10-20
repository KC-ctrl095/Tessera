"""Which weekdays each practitioner works, and making a week of slots."""

from datetime import date, timedelta

import legacy_db

from . import audit, slots
from .errors import Invalid


def working_days(practitioner_id):
    rows = legacy_db.run(
        "SELECT weekday FROM working_days WHERE practitioner_id = ? ORDER BY weekday", practitioner_id
    )
    return [r[0] for r in rows]


def set_working_days(practitioner_id, weekdays):
    days = sorted(set(weekdays))
    if any(not 0 <= d <= 6 for d in days):
        raise Invalid("weekdays are 0 (Monday) to 6 (Sunday)")
    legacy_db.run("DELETE FROM working_days WHERE practitioner_id = ?", practitioner_id, fetch=None)
    for d in days:
        legacy_db.run(
            "INSERT INTO working_days (practitioner_id, weekday) VALUES (?, ?)", practitioner_id, d, fetch=None
        )
    audit.log("schedule.changed", practitioner_id, ",".join(map(str, days)))


def generate_week(practitioner_id, monday):
    """Slots for the practitioner's working days of the week starting `monday` (a date)."""
    if monday.weekday() != 0:
        raise Invalid(f"{monday} isn't a Monday")
    worked = set(working_days(practitioner_id))
    created = 0
    for i in range(7):
        day = monday + timedelta(days=i)
        if day.weekday() in worked:
            created += slots.generate_day(practitioner_id, day.isoformat())
    return created


def this_monday(today: date) -> date:
    return today - timedelta(days=today.weekday())
