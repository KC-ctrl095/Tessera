"""Clinical notes."""

import db

from . import clock
from .errors import Invalid


def add(patient_id, practitioner_id, body):
    if not body or not body.strip():
        raise Invalid("an empty note")
    return db.repo().insert(
        "INSERT INTO notes (patient_id, practitioner_id, written_at, body) VALUES (?, ?, ?, ?)",
        (patient_id, practitioner_id, clock.now_iso(), body.strip()),
    )


def latest(patient_id):
    """The patient's most recent note, or None."""
    row = db.repo().one_or_none(
        "SELECT id, written_at, body, practitioner_id FROM notes WHERE patient_id = ?"
        " ORDER BY written_at DESC, id DESC LIMIT 1",
        (patient_id,),
    )
    if row is None:
        return None
    return {"id": row.id, "written_at": row.written_at, "body": row.body, "by": row.practitioner_id}


def history(patient_id):
    rows = db.repo().all(
        "SELECT n.written_at, pr.name, n.body FROM notes n LEFT JOIN practitioners pr ON pr.id = n.practitioner_id"
        " WHERE n.patient_id = ? ORDER BY n.written_at, n.id",
        (patient_id,),
    )
    return [f"{r.written_at[:10]} {r.name or 'unknown'}: {r.body}" for r in rows]
