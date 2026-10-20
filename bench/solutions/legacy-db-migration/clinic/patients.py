"""Patients: registration, contact details, merging duplicates."""

import db

from . import audit, validation
from .errors import Invalid, NotFound

_COLUMNS = "id, name, birth_date, phone, email, insurer, active"


def _to_dict(row):
    return {
        "id": row.id,
        "name": row.name,
        "birth_date": row.birth_date,
        "phone": row.phone,
        "email": row.email,
        "insurer": row.insurer,
        "active": bool(row.active),
    }


def create(name, birth_date=None, phone=None, email=None, insurer=None):
    name = validation.require_name(name)
    phone = validation.normalise_phone(phone)
    patient_id = db.repo().insert(
        "INSERT INTO patients (name, birth_date, phone, email, insurer) VALUES (?, ?, ?, ?, ?)",
        (name, birth_date, phone, email, insurer),
    )
    audit.log("patient.created", patient_id)
    return patient_id


def get(patient_id):
    row = db.repo().one_or_none(f"SELECT {_COLUMNS} FROM patients WHERE id = ?", (patient_id,))
    if row is None:
        raise NotFound(f"no patient {patient_id}")
    return _to_dict(row)


def update_contact(patient_id, phone=None, email=None):
    changed = db.repo().execute(
        "UPDATE patients SET phone = COALESCE(?, phone), email = COALESCE(?, email) WHERE id = ?",
        (validation.normalise_phone(phone), email, patient_id),
    )
    if changed == 0:
        raise NotFound(f"no patient {patient_id}")
    audit.log("patient.contact", patient_id)


def deactivate(patient_id):
    """True when the patient was active and now isn't."""
    n = db.repo().execute("UPDATE patients SET active = 0 WHERE id = ? AND active = 1", (patient_id,))
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
    repo = db.repo()
    with repo.transaction():
        moved = repo.execute("UPDATE appointments SET patient_id = ? WHERE patient_id = ?", (keep_id, drop_id))
        repo.execute("UPDATE notes SET patient_id = ? WHERE patient_id = ?", (keep_id, drop_id))
        repo.execute("UPDATE invoices SET patient_id = ? WHERE patient_id = ?", (keep_id, drop_id))
        repo.execute("UPDATE waitlist SET patient_id = ? WHERE patient_id = ?", (keep_id, drop_id))
        repo.execute("UPDATE referrals SET patient_id = ? WHERE patient_id = ?", (keep_id, drop_id))
        repo.execute("DELETE FROM patients WHERE id = ?", (drop_id,))
        audit.log("patient.merged", keep_id, f"from {drop_id}")
    return moved
