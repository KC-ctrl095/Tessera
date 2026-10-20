"""Clinic settings, stored as text in the settings table."""

import legacy_db

DEFAULTS = {
    "clinic_name": "Clínica da Ribeira",
    "reminder_time": "18:00",
    "slot_minutes": "30",
}


def get(key):
    row = legacy_db.run("SELECT value FROM settings WHERE key = ?", key, fetch="one")
    if row is None:
        return DEFAULTS.get(key)
    return row[0]


def set(key, value):
    legacy_db.run(
        "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        key,
        str(value),
        fetch=None,
    )


def all_settings():
    out = dict(DEFAULTS)
    out.update(dict(legacy_db.run("SELECT key, value FROM settings")))
    return out
