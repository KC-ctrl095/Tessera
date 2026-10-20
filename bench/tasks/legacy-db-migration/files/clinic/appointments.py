"""Booking, cancelling and moving appointments."""

import legacy_db

from . import audit, clock, patients, reminders
from .errors import Conflict, NotFound


def book(patient_id, slot_id, reason=None):
    """Books a free slot for a patient; returns the appointment id."""
    patients.get(patient_id)
    legacy_db.run("BEGIN", fetch=None)
    try:
        slot = legacy_db.run("SELECT practitioner_id, taken FROM slots WHERE id = ?", slot_id, fetch="one")
        if slot is None:
            raise NotFound(f"no slot {slot_id}")
        if slot[1]:
            raise Conflict(f"slot {slot_id} is taken")
        appointment_id = legacy_db.run(
            "INSERT INTO appointments (patient_id, practitioner_id, slot_id, reason, created_at)"
            " VALUES (?, ?, ?, ?, ?)",
            patient_id,
            slot[0],
            slot_id,
            reason,
            clock.now_iso(),
            fetch="id",
        )
        legacy_db.run("UPDATE slots SET taken = 1 WHERE id = ?", slot_id, fetch=None)
        reminders.schedule(appointment_id)
        audit.log("appointment.booked", appointment_id)
        legacy_db.run("COMMIT", fetch=None)
    except Exception:
        legacy_db.run("ROLLBACK", fetch=None)
        raise
    return appointment_id


def cancel(appointment_id, reason=""):
    legacy_db.run("BEGIN", fetch=None)
    try:
        row = legacy_db.run("SELECT slot_id, status FROM appointments WHERE id = ?", appointment_id, fetch="one")
        if row is None:
            raise NotFound(f"no appointment {appointment_id}")
        slot_id, status = row
        if status != "booked":
            raise Conflict(f"appointment {appointment_id} is {status}")
        legacy_db.run("UPDATE appointments SET status = 'cancelled' WHERE id = ?", appointment_id, fetch=None)
        legacy_db.run("UPDATE slots SET taken = 0 WHERE id = ?", slot_id, fetch=None)
        legacy_db.run("DELETE FROM reminders WHERE appointment_id = ? AND sent = 0", appointment_id, fetch=None)
        audit.log("appointment.cancelled", appointment_id, reason)
        legacy_db.run("COMMIT", fetch=None)
    except Exception:
        legacy_db.run("ROLLBACK", fetch=None)
        raise


def detail(appointment_id):
    row = legacy_db.run(
        "SELECT * FROM appointments a"
        " JOIN patients p ON p.id = a.patient_id"
        " JOIN practitioners pr ON pr.id = a.practitioner_id"
        " JOIN slots s ON s.id = a.slot_id"
        " WHERE a.id = ?",
        appointment_id,
        fetch="one",
    )
    if row is None:
        raise NotFound(f"no appointment {appointment_id}")
    return {
        "id": row[0],
        "status": row[5],
        "reason": row[4],
        "booked_at": row[6],
        "slot_id": row[3],
        "patient": {"id": row[7], "name": row[8], "phone": row[10], "insurer": row[12]},
        "practitioner": {"id": row[14], "name": row[15], "specialty": row[16]},
        "starts_at": row[21],
        "minutes": row[22],
    }


def day_list(practitioner_id, day):
    rows = legacy_db.run(
        "SELECT a.id, s.starts_at, p.name, a.reason, a.status FROM appointments a"
        " JOIN slots s ON s.id = a.slot_id JOIN patients p ON p.id = a.patient_id"
        " WHERE a.practitioner_id = ? AND s.starts_at LIKE ? ORDER BY s.starts_at",
        practitioner_id,
        day + "%",
    )
    return [{"id": r[0], "time": r[1][11:16], "patient": r[2], "reason": r[3] or "", "status": r[4]} for r in rows]


OUTCOMES = ("completed", "no-show")


def set_outcome(appointment_id, outcome):
    """After the appointment time: 'completed' or 'no-show'."""
    if outcome not in OUTCOMES:
        raise ValueError(f"unknown outcome {outcome!r}")
    n = legacy_db.run(
        "UPDATE appointments SET status = ? WHERE id = ? AND status = 'booked'", outcome, appointment_id, fetch=None
    )
    if n == 0:
        raise Conflict(f"appointment {appointment_id} isn't booked")
    audit.log("appointment." + outcome, appointment_id)
