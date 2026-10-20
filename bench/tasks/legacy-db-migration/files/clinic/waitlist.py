"""Patients waiting for a practitioner to have a free slot."""

import legacy_db

from . import appointments, clock
from .errors import Conflict


def add(patient_id, practitioner_id, note=None):
    exists = legacy_db.run(
        "SELECT 1 FROM waitlist WHERE patient_id = ? AND practitioner_id = ?", patient_id, practitioner_id, fetch="one"
    )
    if exists:
        raise Conflict("already on the waiting list")
    return legacy_db.run(
        "INSERT INTO waitlist (patient_id, practitioner_id, added_at, note) VALUES (?, ?, ?, ?)",
        patient_id,
        practitioner_id,
        clock.now_iso(),
        note,
        fetch="id",
    )


def next_in_line(practitioner_id):
    """The entry waiting longest for this practitioner, or None."""
    row = legacy_db.run(
        "SELECT id, patient_id, added_at FROM waitlist WHERE practitioner_id = ? ORDER BY added_at, id",
        practitioner_id,
        fetch="one",
    )
    if row is None:
        return None
    return {"id": row[0], "patient_id": row[1], "added_at": row[2]}


def remove(entry_id):
    return legacy_db.run("DELETE FROM waitlist WHERE id = ?", entry_id, fetch=None) == 1


def offer_slot(practitioner_id, slot_id):
    """Books a freed slot for the next in line; returns the appointment id, or None."""
    nxt = next_in_line(practitioner_id)
    if nxt is None:
        return None
    appointment_id = appointments.book(nxt["patient_id"], slot_id, "from the waiting list")
    remove(nxt["id"])
    return appointment_id
