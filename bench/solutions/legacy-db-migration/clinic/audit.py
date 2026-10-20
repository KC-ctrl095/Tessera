"""The audit log: who changed what, for the clinic's records."""

import db

from . import clock


def log(action, ref_id=None, detail=None):
    return db.repo().insert(
        "INSERT INTO audit_log (at, action, ref_id, detail) VALUES (?, ?, ?, ?)",
        (clock.now_iso(), action, ref_id, detail),
    )


def recent(limit=20):
    rows = db.repo().all("SELECT at, action, ref_id, detail FROM audit_log ORDER BY id DESC LIMIT ?", (limit,))
    return [{"at": r.at, "action": r.action, "ref": r.ref_id, "detail": r.detail} for r in rows]


def count(action):
    return db.repo().scalar("SELECT COUNT(*) FROM audit_log WHERE action = ?", (action,))
