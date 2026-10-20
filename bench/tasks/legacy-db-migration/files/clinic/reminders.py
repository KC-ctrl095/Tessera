"""Appointment reminders (SMS and email), sent the evening before."""

import legacy_db

from . import settings_store, timeutil


def schedule(appointment_id):
    """Queues the reminders of an appointment; returns how many."""
    row = legacy_db.run(
        "SELECT s.starts_at, p.phone, p.email FROM appointments a"
        " JOIN slots s ON s.id = a.slot_id JOIN patients p ON p.id = a.patient_id WHERE a.id = ?",
        appointment_id,
        fetch="one",
    )
    starts_at, phone, email = row
    send_at = timeutil.day_before_at(starts_at, settings_store.get("reminder_time"))
    n = 0
    for channel, to in (("sms", phone), ("email", email)):
        if to:
            legacy_db.run(
                "INSERT INTO reminders (appointment_id, send_at, channel) VALUES (?, ?, ?)",
                appointment_id,
                send_at,
                channel,
                fetch=None,
            )
            n += 1
    return n


def due(now_iso):
    """Unsent reminders whose time has come, oldest first."""
    rows = legacy_db.run(
        "SELECT r.id, r.channel, p.name, p.phone, p.email, s.starts_at FROM reminders r"
        " JOIN appointments a ON a.id = r.appointment_id"
        " JOIN patients p ON p.id = a.patient_id"
        " JOIN slots s ON s.id = a.slot_id"
        " WHERE r.sent = 0 AND r.send_at <= ? ORDER BY r.send_at, r.id",
        now_iso,
    )
    return [
        {"id": r[0], "channel": r[1], "name": r[2], "to": r[3] if r[1] == "sms" else r[4], "starts_at": r[5]}
        for r in rows
    ]


def mark_sent(ids):
    """Marks reminders as sent; returns how many were still unsent."""
    if not ids:
        return 0
    marks = ",".join("?" for _ in ids)
    return legacy_db.run(f"UPDATE reminders SET sent = 1 WHERE sent = 0 AND id IN ({marks})", *ids, fetch=None)


def pending_count():
    return legacy_db.run("SELECT COUNT(*) FROM reminders WHERE sent = 0")[0][0]
