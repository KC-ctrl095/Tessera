"""Numbers for the clinic manager."""

import db


def appointments_per_practitioner(day_from, day_to):
    """{practitioner name: booked or completed appointments between the two days}."""
    rows = db.repo().all(
        "SELECT pr.name, COUNT(s.id) AS n FROM practitioners pr"
        " LEFT JOIN appointments a ON a.practitioner_id = pr.id AND a.status IN ('booked', 'completed')"
        " LEFT JOIN slots s ON s.id = a.slot_id AND substr(s.starts_at, 1, 10) BETWEEN ? AND ?"
        " GROUP BY pr.id ORDER BY pr.name",
        (day_from, day_to),
    )
    return {r.name: r.n for r in rows}


def no_show_rate(practitioner_id):
    row = db.repo().one(
        "SELECT COUNT(*) AS total, SUM(status = 'no-show') AS missed FROM appointments"
        " WHERE practitioner_id = ? AND status IN ('completed', 'no-show')",
        (practitioner_id,),
    )
    return 0.0 if not row.total else round(row.missed / row.total, 3)


def revenue(day_from, day_to):
    return db.repo().scalar(
        "SELECT COALESCE(SUM(amount_cents), 0) FROM payments WHERE paid_on BETWEEN ? AND ?", (day_from, day_to)
    )


def busiest_day():
    """(day, appointments) of the day with the most appointments that aren't cancelled, or None."""
    row = db.repo().one_or_none(
        "SELECT substr(s.starts_at, 1, 10) AS day, COUNT(*) AS n FROM appointments a JOIN slots s ON s.id = a.slot_id"
        " WHERE a.status != 'cancelled' GROUP BY day ORDER BY COUNT(*) DESC, day LIMIT 1"
    )
    return (row.day, row.n) if row else None
