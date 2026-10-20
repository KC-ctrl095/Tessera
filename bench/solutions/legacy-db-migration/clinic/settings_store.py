"""Clinic settings, stored as text in the settings table."""

import db

DEFAULTS = {
    "clinic_name": "Clínica da Ribeira",
    "reminder_time": "18:00",
    "slot_minutes": "30",
}


def get(key):
    row = db.repo().one_or_none("SELECT value FROM settings WHERE key = ?", (key,))
    if row is None:
        return DEFAULTS.get(key)
    return row.value


def set(key, value):
    db.repo().execute(
        "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        (key, str(value)),
    )


def all_settings():
    out = dict(DEFAULTS)
    out.update({r.key: r.value for r in db.repo().all("SELECT key, value FROM settings")})
    return out
