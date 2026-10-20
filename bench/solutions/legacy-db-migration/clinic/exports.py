"""CSV and text exports for the front desk and the accountant."""

import csv
import io

import db

_PATIENT_COLUMNS = ["id", "name", "birth_date", "phone", "email", "insurer", "active"]


def patients_csv():
    """Every patient, all columns, with a header row."""
    rows = db.repo().all(f"SELECT {', '.join(_PATIENT_COLUMNS)} FROM patients ORDER BY id")
    out = io.StringIO()
    writer = csv.writer(out, lineterminator="\n")
    writer.writerow(_PATIENT_COLUMNS)
    writer.writerows([r[c] for c in _PATIENT_COLUMNS] for r in rows)
    return out.getvalue()


def day_sheet(day):
    """The printed list of a day's booked appointments."""
    rows = db.repo().all(
        "SELECT s.starts_at, pr.name AS doctor, p.name AS patient, a.reason FROM appointments a"
        " JOIN slots s ON s.id = a.slot_id"
        " JOIN practitioners pr ON pr.id = a.practitioner_id"
        " JOIN patients p ON p.id = a.patient_id"
        " WHERE s.starts_at LIKE ? AND a.status = 'booked' ORDER BY s.starts_at, pr.name",
        (day + "%",),
    )
    lines = [f"{r.starts_at[11:16]}  {r.doctor:<22} {r.patient:<24} {r.reason or ''}".rstrip() for r in rows]
    return "\n".join([f"Appointments {day}"] + lines)
