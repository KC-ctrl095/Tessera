"""Appointment reminders (SMS and email), sent the evening before."""

import db

from . import settings_store, timeutil


def schedule(appointment_id):
    """Queues the reminders of an appointment; returns how many."""
    repo = db.repo()
    row = repo.one(
        "SELECT s.starts_at, p.phone, p.email FROM appointments a"
        " JOIN slots s ON s.id = a.slot_id JOIN patients p ON p.id = a.patient_id WHERE a.id = ?",
        (appointment_id,),
    )
    send_at = timeutil.day_before_at(row.starts_at, settings_store.get("reminder_time"))
    n = 0
    for channel, to in (("sms", row.phone), ("email", row.email)):
        if to:
            repo.execute(
                "INSERT INTO reminders (appointment_id, send_at, channel) VALUES (?, ?, ?)",
                (appointment_id, send_at, channel),
            )
            n += 1
    return n


def due(now_iso):
    """Unsent reminders whose time has come, oldest first."""
    rows = db.repo().all(
        "SELECT r.id, r.channel, p.name, p.phone, p.email, s.starts_at FROM reminders r"
        " JOIN appointments a ON a.id = r.appointment_id"
        " JOIN patients p ON p.id = a.patient_id"
        " JOIN slots s ON s.id = a.slot_id"
        " WHERE r.sent = 0 AND r.send_at <= ? ORDER BY r.send_at, r.id",
        (now_iso,),
    )
    return [
        {"id": r.id, "channel": r.channel, "name": r.name, "to": r.phone if r.channel == "sms" else r.email, "starts_at": r.starts_at}
        for r in rows
    ]


def mark_sent(ids):
    """Marks reminders as sent; returns how many were still unsent."""
    if not ids:
        return 0
    marks = ",".join("?" for _ in ids)
    return db.repo().execute(f"UPDATE reminders SET sent = 1 WHERE sent = 0 AND id IN ({marks})", tuple(ids))


def pending_count():
    return db.repo().scalar("SELECT COUNT(*) FROM reminders WHERE sent = 0")
