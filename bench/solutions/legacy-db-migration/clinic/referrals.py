"""Referrals from one practitioner to a specialty."""

import db

from . import clock


def create(patient_id, from_practitioner, to_specialty):
    return db.repo().insert(
        "INSERT INTO referrals (patient_id, from_practitioner, to_specialty, created_on) VALUES (?, ?, ?, ?)",
        (patient_id, from_practitioner, to_specialty, clock.today().isoformat()),
    )


def open_for(specialty):
    rows = db.repo().all(
        "SELECT r.id, p.name, r.created_on FROM referrals r JOIN patients p ON p.id = r.patient_id"
        " WHERE r.to_specialty = ? AND r.status = 'open' ORDER BY r.created_on, r.id",
        (specialty,),
    )
    return [{"id": r.id, "patient": r.name, "since": r.created_on} for r in rows]


def oldest_open():
    """(referral id, specialty) of the referral waiting longest, or None."""
    row = db.repo().one_or_none(
        "SELECT id, to_specialty FROM referrals WHERE status = 'open' ORDER BY created_on, id LIMIT 1"
    )
    return None if row is None else (row.id, row.to_specialty)
