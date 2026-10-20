"""Doctors, physiotherapists and the other practitioners."""

import db

from . import audit, clock
from .errors import NotFound


def list_active():
    rows = db.repo().all("SELECT id, name, specialty, room_id FROM practitioners WHERE active = 1 ORDER BY name")
    return [{"id": r.id, "name": r.name, "specialty": r.specialty, "room_id": r.room_id} for r in rows]


def get(practitioner_id):
    row = db.repo().one_or_none(
        "SELECT id, name, specialty, room_id, active FROM practitioners WHERE id = ?", (practitioner_id,)
    )
    if not row:
        raise NotFound(f"no practitioner {practitioner_id}")
    return {"id": row.id, "name": row.name, "specialty": row.specialty, "room_id": row.room_id, "active": bool(row.active)}


def retire(practitioner_id):
    """Deactivates a practitioner and removes their free future slots; returns how many."""
    get(practitioner_id)
    repo = db.repo()
    repo.execute("UPDATE practitioners SET active = 0 WHERE id = ?", (practitioner_id,))
    removed = repo.execute(
        "DELETE FROM slots WHERE practitioner_id = ? AND taken = 0 AND starts_at >= ?",
        (practitioner_id, clock.now_iso()),
    )
    audit.log("practitioner.retired", practitioner_id, f"{removed} free slots removed")
    return removed
