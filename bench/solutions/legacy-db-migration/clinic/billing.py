"""Invoices for completed appointments, and payments."""

import db

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
    repo = db.repo()
    row = repo.one_or_none(
        "SELECT a.patient_id, a.status, pr.specialty, p.insurer FROM appointments a"
        " JOIN practitioners pr ON pr.id = a.practitioner_id JOIN patients p ON p.id = a.patient_id"
        " WHERE a.id = ?",
        (appointment_id,),
    )
    if row is None:
        raise NotFound(f"no appointment {appointment_id}")
    if row.status != "completed":
        raise Conflict(f"appointment {appointment_id} is {row.status}")
    existing = repo.one_or_none("SELECT id FROM invoices WHERE appointment_id = ?", (appointment_id,))
    if existing:
        return existing.id
    amount = patient_share(row.insurer, FEES[row.specialty])
    return repo.insert(
        "INSERT INTO invoices (appointment_id, patient_id, amount_cents, issued_on) VALUES (?, ?, ?, ?)",
        (appointment_id, row.patient_id, amount, clock.today().isoformat()),
    )


def record_payment(invoice_id, amount_cents, method="card"):
    """Records a payment; the invoice is paid once payments cover it. Returns the total paid."""
    if amount_cents <= 0:
        raise ValueError("amount must be positive")
    repo = db.repo()
    with repo.transaction():
        inv = repo.one_or_none("SELECT amount_cents, status FROM invoices WHERE id = ?", (invoice_id,))
        if inv is None:
            raise NotFound(f"no invoice {invoice_id}")
        if inv.status == "void":
            raise Conflict(f"invoice {invoice_id} is void")
        repo.execute(
            "INSERT INTO payments (invoice_id, amount_cents, paid_on, method) VALUES (?, ?, ?, ?)",
            (invoice_id, amount_cents, clock.today().isoformat(), method),
        )
        paid = repo.scalar("SELECT COALESCE(SUM(amount_cents), 0) FROM payments WHERE invoice_id = ?", (invoice_id,))
        if paid >= inv.amount_cents:
            repo.execute("UPDATE invoices SET status = 'paid' WHERE id = ?", (invoice_id,))
        audit.log("invoice.payment", invoice_id, f"{amount_cents} by {method}")
    return paid


def balance(patient_id):
    """What the patient still owes on open invoices, in cents."""
    rows = db.repo().all(
        "SELECT i.amount_cents,"
        " COALESCE((SELECT SUM(p.amount_cents) FROM payments p WHERE p.invoice_id = i.id), 0) AS paid"
        " FROM invoices i WHERE i.patient_id = ? AND i.status = 'open'",
        (patient_id,),
    )
    return sum(r.amount_cents - r.paid for r in rows)
