"""Clinical notes."""

import legacy_db

from . import clock
from .errors import Invalid


def add(patient_id, practitioner_id, body):
    if not body or not body.strip():
        raise Invalid("an empty note")
    return legacy_db.run(
        "INSERT INTO notes (patient_id, practitioner_id, written_at, body) VALUES (?, ?, ?, ?)",
        patient_id,
        practitioner_id,
        clock.now_iso(),
        body.strip(),
        fetch="id",
    )


def latest(patient_id):
    """The patient's most recent note, or None."""
    row = legacy_db.run(
        "SELECT id, written_at, body, practitioner_id FROM notes WHERE patient_id = ? ORDER BY written_at DESC, id DESC",
        patient_id,
        fetch="one",
    )
    if row is None:
        return None
    return {"id": row[0], "written_at": row[1], "body": row[2], "by": row[3]}


def history(patient_id):
    rows = legacy_db.run(
        "SELECT n.written_at, pr.name, n.body FROM notes n LEFT JOIN practitioners pr ON pr.id = n.practitioner_id"
        " WHERE n.patient_id = ? ORDER BY n.written_at, n.id",
        patient_id,
    )
    return [f"{written[:10]} {name or 'unknown'}: {body}" for written, name, body in rows]
