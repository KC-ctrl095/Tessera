"""CSV and text exports for the front desk and the accountant."""

import csv
import io

import legacy_db


def patients_csv():
    """Every patient, all columns, with a header row."""
    rows = legacy_db.run("SELECT * FROM patients ORDER BY id")
    columns = [d[0] for d in legacy_db.connection().execute("SELECT * FROM patients LIMIT 0").description]
    out = io.StringIO()
    writer = csv.writer(out, lineterminator="\n")
    writer.writerow(columns)
    writer.writerows(rows)
    return out.getvalue()


def day_sheet(day):
    """The printed list of a day's booked appointments."""
    rows = legacy_db.run(
        "SELECT s.starts_at, pr.name, p.name, a.reason FROM appointments a"
        " JOIN slots s ON s.id = a.slot_id"
        " JOIN practitioners pr ON pr.id = a.practitioner_id"
        " JOIN patients p ON p.id = a.patient_id"
        " WHERE s.starts_at LIKE ? AND a.status = 'booked' ORDER BY s.starts_at, pr.name",
        day + "%",
    )
    lines = [f"{starts[11:16]}  {doctor:<22} {patient:<24} {reason or ''}".rstrip() for starts, doctor, patient, reason in rows]
    return "\n".join([f"Appointments {day}"] + lines)
