"""Staff accounts and what each role may do."""

import sqlite3

import legacy_db

from .errors import Conflict, Invalid

ROLES = ("reception", "practitioner", "admin")
ALLOWED = {
    "reception": {"book", "cancel", "register"},
    "practitioner": {"book", "cancel", "notes", "prescribe", "refer"},
    "admin": {"book", "cancel", "register", "notes", "prescribe", "refer", "billing", "settings"},
}


def add(login, name, role):
    if role not in ROLES:
        raise Invalid(f"unknown role {role!r}")
    try:
        return legacy_db.run(
            "INSERT INTO staff (login, name, role) VALUES (?, ?, ?)", login.lower(), name, role, fetch="id"
        )
    except sqlite3.IntegrityError:
        raise Conflict(f"login {login!r} is taken") from None


def list_staff():
    rows = legacy_db.run("SELECT login, name, role FROM staff ORDER BY role, login")
    return [" / ".join(r) for r in rows]
