"""The audit log: who changed what, for the clinic's records."""

import legacy_db

from . import clock


def log(action, ref_id=None, detail=None):
    return legacy_db.run(
        "INSERT INTO audit_log (at, action, ref_id, detail) VALUES (?, ?, ?, ?)",
        clock.now_iso(),
        action,
        ref_id,
        detail,
        fetch="id",
    )


def recent(limit=20):
    rows = legacy_db.run("SELECT at, action, ref_id, detail FROM audit_log ORDER BY id DESC LIMIT ?", limit)
    return [{"at": r[0], "action": r[1], "ref": r[2], "detail": r[3]} for r in rows]


def count(action):
    return legacy_db.run("SELECT COUNT(*) FROM audit_log WHERE action = ?", action, fetch="one")[0]
