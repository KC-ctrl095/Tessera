"""Invoices for completed appointments, and payments."""

import legacy_db

from . import audit, clock
from .errors import Conflict, NotFound

FEES = {"general": 4500, "physio": 3800, "dermatology": 6500, "paediatrics": 4000, "nutrition": 3500}
# Percent of the fee each insurer covers.
COVER = {"Medis": 70, "Multicare": 80, "AdvanceCare": 60}


def patient_share(insurer, fee_cents):
    """What the patient pays of a fee, after their insurer's part."""
    return fee_cents - fee_cents * COVER.get(insurer, 0) // 100


def invoice_for(appointment_id):
    """The invoice of a completed appointment (made on first call); returns its id."""
    row = legacy_db.run(
        "SELECT a.patient_id, a.status, pr.specialty, p.insurer FROM appointments a"
        " JOIN practitioners pr ON pr.id = a.practitioner_id JOIN patients p ON p.id = a.patient_id"
        " WHERE a.id = ?",
        appointment_id,
        fetch="one",
    )
    if row is None:
        raise NotFound(f"no appointment {appointment_id}")
    patient_id, status, specialty, insurer = row
    if status != "completed":
        raise Conflict(f"appointment {appointment_id} is {status}")
    existing = legacy_db.run("SELECT id FROM invoices WHERE appointment_id = ?", appointment_id, fetch="one")
    if existing:
        return existing[0]
    amount = patient_share(insurer, FEES[specialty])
    return legacy_db.run(
        "INSERT INTO invoices (appointment_id, patient_id, amount_cents, issued_on) VALUES (?, ?, ?, ?)",
        appointment_id,
        patient_id,
        amount,
        clock.today().isoformat(),
        fetch="id",
    )


def record_payment(invoice_id, amount_cents, method="card"):
    """Records a payment; the invoice is paid once payments cover it. Returns the total paid."""
    if amount_cents <= 0:
        raise ValueError("amount must be positive")
    legacy_db.run("BEGIN", fetch=None)
    try:
        inv = legacy_db.run("SELECT amount_cents, status FROM invoices WHERE id = ?", invoice_id, fetch="one")
        if inv is None:
            raise NotFound(f"no invoice {invoice_id}")
        if inv[1] == "void":
            raise Conflict(f"invoice {invoice_id} is void")
        legacy_db.run(
            "INSERT INTO payments (invoice_id, amount_cents, paid_on, method) VALUES (?, ?, ?, ?)",
            invoice_id,
            amount_cents,
            clock.today().isoformat(),
            method,
            fetch=None,
        )
        paid = legacy_db.run(
            "SELECT COALESCE(SUM(amount_cents), 0) FROM payments WHERE invoice_id = ?", invoice_id, fetch="one"
        )[0]
        if paid >= inv[0]:
            legacy_db.run("UPDATE invoices SET status = 'paid' WHERE id = ?", invoice_id, fetch=None)
        audit.log("invoice.payment", invoice_id, f"{amount_cents} by {method}")
        legacy_db.run("COMMIT", fetch=None)
    except Exception:
        legacy_db.run("ROLLBACK", fetch=None)
        raise
    return paid


def balance(patient_id):
    """What the patient still owes on open invoices, in cents."""
    rows = legacy_db.run(
        "SELECT i.amount_cents, COALESCE((SELECT SUM(p.amount_cents) FROM payments p WHERE p.invoice_id = i.id), 0)"
        " FROM invoices i WHERE i.patient_id = ? AND i.status = 'open'",
        patient_id,
    )
    return sum(r[0] - r[1] for r in rows)
