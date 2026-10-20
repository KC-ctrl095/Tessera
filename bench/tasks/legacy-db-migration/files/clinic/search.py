"""Finding patients at the front desk."""

import legacy_db

from . import validation


def by_name(text, limit=20):
    pattern = "%" + text.strip().lower() + "%"
    rows = legacy_db.run(
        "SELECT id, name, phone FROM patients WHERE lower(name) LIKE ? AND active = 1 ORDER BY name, id LIMIT ?",
        pattern,
        limit,
    )
    return [{"id": pid, "name": name, "phone": phone} for pid, name, phone in rows]


def by_phone(phone):
    """The patient registered first with this number (families often share one), or None."""
    row = legacy_db.run(
        "SELECT id FROM patients WHERE phone = ? ORDER BY id", validation.normalise_phone(phone), fetch="one"
    )
    return row[0] if row else None


def names_of(ids):
    """[(id, name)] for the given patient ids, by id."""
    if not ids:
        return []
    marks = ", ".join("?" for _ in ids)
    rows = legacy_db.run(f"SELECT id, name FROM patients WHERE id IN ({marks}) ORDER BY id", *ids)
    return [(r[0], r[1]) for r in rows]
