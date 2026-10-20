"""Finding patients at the front desk."""

import db

from . import validation


def by_name(text, limit=20):
    pattern = "%" + text.strip().lower() + "%"
    rows = db.repo().all(
        "SELECT id, name, phone FROM patients WHERE lower(name) LIKE ? AND active = 1 ORDER BY name, id LIMIT ?",
        (pattern, limit),
    )
    return [{"id": r.id, "name": r.name, "phone": r.phone} for r in rows]


def by_phone(phone):
    """The patient registered first with this number (families often share one), or None."""
    row = db.repo().one_or_none(
        "SELECT id FROM patients WHERE phone = ? ORDER BY id LIMIT 1", (validation.normalise_phone(phone),)
    )
    return row.id if row else None


def names_of(ids):
    """[(id, name)] for the given patient ids, by id."""
    if not ids:
        return []
    marks = ", ".join("?" for _ in ids)
    rows = db.repo().all(f"SELECT id, name FROM patients WHERE id IN ({marks}) ORDER BY id", tuple(ids))
    return [(r.id, r.name) for r in rows]
