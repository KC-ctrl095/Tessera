"""Referrals from one practitioner to a specialty."""

import legacy_db

from . import clock


def create(patient_id, from_practitioner, to_specialty):
    return legacy_db.run(
        "INSERT INTO referrals (patient_id, from_practitioner, to_specialty, created_on) VALUES (?, ?, ?, ?)",
        patient_id,
        from_practitioner,
        to_specialty,
        clock.today().isoformat(),
        fetch="id",
    )


def open_for(specialty):
    rows = legacy_db.run(
        "SELECT r.id, p.name, r.created_on FROM referrals r JOIN patients p ON p.id = r.patient_id"
        " WHERE r.to_specialty = ? AND r.status = 'open' ORDER BY r.created_on, r.id",
        specialty,
    )
    return [{"id": i, "patient": name, "since": created} for i, name, created in rows]


def oldest_open():
    """(referral id, specialty) of the referral waiting longest, or None."""
    row = legacy_db.run(
        "SELECT id, to_specialty FROM referrals WHERE status = 'open' ORDER BY created_on, id", fetch="one"
    )
    return None if row is None else (row[0], row[1])
