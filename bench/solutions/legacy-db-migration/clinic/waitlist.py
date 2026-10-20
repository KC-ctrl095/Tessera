"""Patients waiting for a practitioner to have a free slot."""

import db

from . import appointments, clock
from .errors import Conflict


def add(patient_id, practitioner_id, note=None):
    repo = db.repo()
    exists = repo.one_or_none(
        "SELECT 1 AS found FROM waitlist WHERE patient_id = ? AND practitioner_id = ?", (patient_id, practitioner_id)
    )
    if exists:
        raise Conflict("already on the waiting list")
    return repo.insert(
        "INSERT INTO waitlist (patient_id, practitioner_id, added_at, note) VALUES (?, ?, ?, ?)",
        (patient_id, practitioner_id, clock.now_iso(), note),
    )


def next_in_line(practitioner_id):
    """The entry waiting longest for this practitioner, or None."""
    row = db.repo().one_or_none(
        "SELECT id, patient_id, added_at FROM waitlist WHERE practitioner_id = ? ORDER BY added_at, id LIMIT 1",
        (practitioner_id,),
    )
    if row is None:
        return None
    return {"id": row.id, "patient_id": row.patient_id, "added_at": row.added_at}


def remove(entry_id):
    return db.repo().execute("DELETE FROM waitlist WHERE id = ?", (entry_id,)) == 1


def offer_slot(practitioner_id, slot_id):
    """Books a freed slot for the next in line; returns the appointment id, or None."""
    nxt = next_in_line(practitioner_id)
    if nxt is None:
        return None
    appointment_id = appointments.book(nxt["patient_id"], slot_id, "from the waiting list")
    remove(nxt["id"])
    return appointment_id
