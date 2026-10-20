"""Booking, cancelling and moving appointments."""

import db

from . import audit, clock, patients, reminders
from .errors import Conflict, NotFound


def book(patient_id, slot_id, reason=None):
    """Books a free slot for a patient; returns the appointment id."""
    patients.get(patient_id)
    repo = db.repo()
    with repo.transaction():
        slot = repo.one_or_none("SELECT practitioner_id, taken FROM slots WHERE id = ?", (slot_id,))
        if slot is None:
            raise NotFound(f"no slot {slot_id}")
        if slot.taken:
            raise Conflict(f"slot {slot_id} is taken")
        appointment_id = repo.insert(
            "INSERT INTO appointments (patient_id, practitioner_id, slot_id, reason, created_at)"
            " VALUES (?, ?, ?, ?, ?)",
            (patient_id, slot.practitioner_id, slot_id, reason, clock.now_iso()),
        )
        repo.execute("UPDATE slots SET taken = 1 WHERE id = ?", (slot_id,))
        reminders.schedule(appointment_id)
        audit.log("appointment.booked", appointment_id)
    return appointment_id


def cancel(appointment_id, reason=""):
    repo = db.repo()
    with repo.transaction():
        row = repo.one_or_none("SELECT slot_id, status FROM appointments WHERE id = ?", (appointment_id,))
        if row is None:
            raise NotFound(f"no appointment {appointment_id}")
        if row.status != "booked":
            raise Conflict(f"appointment {appointment_id} is {row.status}")
        repo.execute("UPDATE appointments SET status = 'cancelled' WHERE id = ?", (appointment_id,))
        repo.execute("UPDATE slots SET taken = 0 WHERE id = ?", (row.slot_id,))
        repo.execute("DELETE FROM reminders WHERE appointment_id = ? AND sent = 0", (appointment_id,))
        audit.log("appointment.cancelled", appointment_id, reason)


def detail(appointment_id):
    row = db.repo().one_or_none(
        "SELECT a.id, a.status, a.reason, a.created_at, a.slot_id,"
        " p.id AS patient_id, p.name AS patient_name, p.phone AS patient_phone, p.insurer AS patient_insurer,"
        " pr.id AS practitioner_id, pr.name AS practitioner_name, pr.specialty,"
        " s.starts_at, s.minutes"
        " FROM appointments a"
        " JOIN patients p ON p.id = a.patient_id"
        " JOIN practitioners pr ON pr.id = a.practitioner_id"
        " JOIN slots s ON s.id = a.slot_id"
        " WHERE a.id = ?",
        (appointment_id,),
    )
    if row is None:
        raise NotFound(f"no appointment {appointment_id}")
    return {
        "id": row.id,
        "status": row.status,
        "reason": row.reason,
        "booked_at": row.created_at,
        "slot_id": row.slot_id,
        "patient": {"id": row.patient_id, "name": row.patient_name, "phone": row.patient_phone, "insurer": row.patient_insurer},
        "practitioner": {"id": row.practitioner_id, "name": row.practitioner_name, "specialty": row.specialty},
        "starts_at": row.starts_at,
        "minutes": row.minutes,
    }


def day_list(practitioner_id, day):
    rows = db.repo().all(
        "SELECT a.id, s.starts_at, p.name, a.reason, a.status FROM appointments a"
        " JOIN slots s ON s.id = a.slot_id JOIN patients p ON p.id = a.patient_id"
        " WHERE a.practitioner_id = ? AND s.starts_at LIKE ? ORDER BY s.starts_at",
        (practitioner_id, day + "%"),
    )
    return [
        {"id": r.id, "time": r.starts_at[11:16], "patient": r.name, "reason": r.reason or "", "status": r.status}
        for r in rows
    ]


OUTCOMES = ("completed", "no-show")


def set_outcome(appointment_id, outcome):
    """After the appointment time: 'completed' or 'no-show'."""
    if outcome not in OUTCOMES:
        raise ValueError(f"unknown outcome {outcome!r}")
    n = db.repo().execute(
        "UPDATE appointments SET status = ? WHERE id = ? AND status = 'booked'", (outcome, appointment_id)
    )
    if n == 0:
        raise Conflict(f"appointment {appointment_id} isn't booked")
    audit.log("appointment." + outcome, appointment_id)
