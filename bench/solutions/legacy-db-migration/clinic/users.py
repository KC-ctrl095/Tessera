"""Staff accounts and what each role may do."""

import db

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
        return db.repo().insert("INSERT INTO staff (login, name, role) VALUES (?, ?, ?)", (login.lower(), name, role))
    except db.IntegrityError:
        raise Conflict(f"login {login!r} is taken") from None


def list_staff():
    rows = db.repo().all("SELECT login, name, role FROM staff ORDER BY role, login")
    return [f"{r.login} / {r.name} / {r.role}" for r in rows]
