"""Patients: registration, contact details, merging duplicates."""

import legacy_db

from . import audit, validation
from .errors import Invalid, NotFound

_COLUMNS = "id, name, birth_date, phone, email, insurer, active"


def _to_dict(row):
    return {
        "id": row[0],
        "name": row[1],
        "birth_date": row[2],
        "phone": row[3],
        "email": row[4],
        "insurer": row[5],
        "active": bool(row[6]),
    }


def create(name, birth_date=None, phone=None, email=None, insurer=None):
    name = validation.require_name(name)
    phone = validation.normalise_phone(phone)
    patient_id = legacy_db.run(
        "INSERT INTO patients (name, birth_date, phone, email, insurer) VALUES (?, ?, ?, ?, ?)",
        name,
        birth_date,
        phone,
        email,
        insurer,
        fetch="id",
    )
    audit.log("patient.created", patient_id)
    return patient_id


def get(patient_id):
    row = legacy_db.run(f"SELECT {_COLUMNS} FROM patients WHERE id = ?", patient_id, fetch="one")
    if row is None:
        raise NotFound(f"no patient {patient_id}")
    return _to_dict(row)


def update_contact(patient_id, phone=None, email=None):
    changed = legacy_db.run(
        "UPDATE patients SET phone = COALESCE(?, phone), email = COALESCE(?, email) WHERE id = ?",
        validation.normalise_phone(phone),
        email,
        patient_id,
        fetch=None,
    )
    if changed == 0:
        raise NotFound(f"no patient {patient_id}")
    audit.log("patient.contact", patient_id)


def deactivate(patient_id):
    """True when the patient was active and now isn't."""
    n = legacy_db.run("UPDATE patients SET active = 0 WHERE id = ? AND active = 1", patient_id, fetch=None)
    if n:
        audit.log("patient.deactivated", patient_id)
    return n == 1


def merge(keep_id, drop_id):
    """Moves everything of `drop_id` to `keep_id` and deletes `drop_id`.

    Returns the number of appointments moved.
    """
    if keep_id == drop_id:
        raise Invalid("can't merge a patient with themselves")
    get(keep_id)
    get(drop_id)
    legacy_db.run("BEGIN", fetch=None)
    try:
        moved = legacy_db.run(
            "UPDATE appointments SET patient_id = ? WHERE patient_id = ?", keep_id, drop_id, fetch=None
        )
        legacy_db.run("UPDATE notes SET patient_id = ? WHERE patient_id = ?", keep_id, drop_id, fetch=None)
        legacy_db.run("UPDATE invoices SET patient_id = ? WHERE patient_id = ?", keep_id, drop_id, fetch=None)
        legacy_db.run("UPDATE waitlist SET patient_id = ? WHERE patient_id = ?", keep_id, drop_id, fetch=None)
        legacy_db.run("UPDATE referrals SET patient_id = ? WHERE patient_id = ?", keep_id, drop_id, fetch=None)
        legacy_db.run("DELETE FROM patients WHERE id = ?", drop_id, fetch=None)
        audit.log("patient.merged", keep_id, f"from {drop_id}")
        legacy_db.run("COMMIT", fetch=None)
    except Exception:
        legacy_db.run("ROLLBACK", fetch=None)
        raise
    return moved
