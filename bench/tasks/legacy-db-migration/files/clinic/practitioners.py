"""Doctors, physiotherapists and the other practitioners."""

import legacy_db

from . import audit, clock
from .errors import NotFound


def list_active():
    rows = legacy_db.run("SELECT id, name, specialty, room_id FROM practitioners WHERE active = 1 ORDER BY name")
    return [{"id": r[0], "name": r[1], "specialty": r[2], "room_id": r[3]} for r in rows]


def get(practitioner_id):
    row = legacy_db.run(
        "SELECT id, name, specialty, room_id, active FROM practitioners WHERE id = ?", practitioner_id, fetch="one"
    )
    if not row:
        raise NotFound(f"no practitioner {practitioner_id}")
    return {"id": row[0], "name": row[1], "specialty": row[2], "room_id": row[3], "active": bool(row[4])}


def retire(practitioner_id):
    """Deactivates a practitioner and removes their free future slots; returns how many."""
    get(practitioner_id)
    legacy_db.run("UPDATE practitioners SET active = 0 WHERE id = ?", practitioner_id, fetch=None)
    removed = legacy_db.run(
        "DELETE FROM slots WHERE practitioner_id = ? AND taken = 0 AND starts_at >= ?",
        practitioner_id,
        clock.now_iso(),
        fetch=None,
    )
    audit.log("practitioner.retired", practitioner_id, f"{removed} free slots removed")
    return removed
