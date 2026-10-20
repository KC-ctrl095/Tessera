"""JSON shapes. Instants go out as Lisbon local time with their offset."""

from core.money import format_eur
from utils.tz import format_offset, to_local


def local_iso(utc) -> str:
    """'2026-07-01T10:00:00Z' -> '2026-07-01T11:00:00+01:00'."""
    return to_local(utc).strftime("%Y-%m-%dT%H:%M:%S") + format_offset(utc)


def payment_json(p) -> dict:
    return {
        "id": p.id,
        "subscription": p.subscription_id,
        "customer": p.customer_id,
        "plan": p.plan_code,
        "amount": format_eur(p.amount_cents),
        "amount_cents": p.amount_cents,
        "paid_at": local_iso(p.paid_at),
        "kind": p.kind,
    }


def invoice_json(inv) -> dict:
    return {
        "number": inv.number,
        "payment": inv.payment_id,
        "issued_at": local_iso(inv.issued_at),
        "due_at": local_iso(inv.due_at),
        "net": format_eur(inv.net_cents),
        "vat": format_eur(inv.vat_cents),
        "gross": format_eur(inv.gross_cents),
        "vat_rate": inv.vat_rate,
    }


def report_json(r) -> dict:
    return {
        "month": r.label,
        "total_cents": r.total_cents,
        "total": format_eur(r.total_cents),
        "payments": r.payment_count,
        "refunds_cents": r.refunds_cents,
        "paying_customers": r.paying_customers,
        "by_plan": r.by_plan,
    }
