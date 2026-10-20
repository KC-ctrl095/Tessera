"""Numbers for the clinic manager."""

import legacy_db


def appointments_per_practitioner(day_from, day_to):
    """{practitioner name: booked or completed appointments between the two days}."""
    rows = legacy_db.run(
        "SELECT pr.name, COUNT(s.id) FROM practitioners pr"
        " LEFT JOIN appointments a ON a.practitioner_id = pr.id AND a.status IN ('booked', 'completed')"
        " LEFT JOIN slots s ON s.id = a.slot_id AND substr(s.starts_at, 1, 10) BETWEEN ? AND ?"
        " GROUP BY pr.id ORDER BY pr.name",
        day_from,
        day_to,
    )
    return {r[0]: r[1] for r in rows}


def no_show_rate(practitioner_id):
    total, missed = legacy_db.run(
        "SELECT COUNT(*), SUM(status = 'no-show') FROM appointments"
        " WHERE practitioner_id = ? AND status IN ('completed', 'no-show')",
        practitioner_id,
        fetch="one",
    )
    return 0.0 if not total else round(missed / total, 3)


def revenue(day_from, day_to):
    return legacy_db.run(
        "SELECT COALESCE(SUM(amount_cents), 0) FROM payments WHERE paid_on BETWEEN ? AND ?",
        day_from,
        day_to,
        fetch="one",
    )[0]


def busiest_day():
    """(day, appointments) of the day with the most appointments that aren't cancelled, or None."""
    row = legacy_db.run(
        "SELECT substr(s.starts_at, 1, 10) AS day, COUNT(*) FROM appointments a JOIN slots s ON s.id = a.slot_id"
        " WHERE a.status != 'cancelled' GROUP BY day ORDER BY COUNT(*) DESC, day",
        fetch="one",
    )
    return (row[0], row[1]) if row else None
