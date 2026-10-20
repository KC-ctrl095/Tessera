"""Demo data: a week at Clínica da Ribeira (the tests use it too)."""

from datetime import date, datetime

import db

from . import (
    appointments,
    billing,
    clock,
    notes,
    outbox,
    patients,
    referrals,
    schedules,
    settings_store,
    users,
    waitlist,
)

ROOMS = [(1, "Sala 1", 0), (2, "Sala 2", 0), (3, "Sala 3", 1), (4, "Ginásio", 1)]
PRACTITIONERS = [
    (1, "Dr.ª Helena Vaz", "general", 1, [0, 1, 2, 3, 4]),
    (2, "Dr. Nuno Quaresma", "general", 2, [0, 2, 4]),
    (3, "Rita Paiva", "physio", 4, [0, 1, 3]),
    (4, "Dr.ª Sofia Lacerda", "dermatology", 3, [1, 3]),
    (5, "Dr. André Seabra", "paediatrics", 2, [1, 3]),
    (6, "Marta Couto", "nutrition", None, [4]),
]
PATIENTS = [
    ("Joana Matos", "1984-03-12", "+351 912 400 101", "joana.matos@mail.example", "Medis"),
    ("Tiago Matos", "2015-07-30", "+351 912 400 101", None, "Medis"),
    ("Albertina Rocha", "1941-11-02", "+351 253 118 220", None, None),
    ("Pedro Gaspar", "1990-05-21", "+351 936 220 418", "pgaspar@mail.example", "Multicare"),
    ("Leonor Bastos", "2001-01-09", None, "leonor.b@mail.example", None),
    ("Mário Fontes", "1958-09-14", "+351 917 551 032", "mario.fontes@mail.example", "AdvanceCare"),
    ("Inês Carrilho", "1977-12-03", "+351 961 300 777", "ines.carrilho@mail.example", "Multicare"),
    ("Rafael Simões", "2019-04-17", "+351 965 812 044", "simoes.familia@mail.example", None),
    ("Beatriz Lobo", "1995-08-25", "+351 918 004 562", None, "Medis"),
    ("Carlos Pinheiro", "1966-02-28", "+351 939 711 245", "c.pinheiro@mail.example", None),
    ("Graça Esteves", "1949-06-06", "+351 222 405 916", None, "AdvanceCare"),
    ("Duarte Macedo", "1988-10-10", "+351 927 144 380", "duarte.macedo@mail.example", "Multicare"),
    ("Joana Matos", "1984-03-12", "912400101", None, "Medis"),
    ("Filipa Antunes", "2008-03-01", "+351 914 909 113", "filipa.a@mail.example", None),
]
HOLIDAYS = [
    ("2026-11-01", "Todos os Santos"),
    ("2026-12-01", "Restauração da Independência"),
    ("2026-12-08", "Imaculada Conceição"),
    ("2026-12-25", "Natal"),
]


def _at(day, hhmm):
    return datetime.fromisoformat(f"{day}T{hhmm}")


def _slot(practitioner_id, starts_at):
    return db.repo().one(
        "SELECT id FROM slots WHERE practitioner_id = ? AND starts_at = ?", (practitioner_id, starts_at)
    ).id


def load():
    repo = db.repo()
    for room in ROOMS:
        repo.execute("INSERT INTO rooms (id, name, floor) VALUES (?, ?, ?)", room)
    for pid, name, specialty, room, weekdays in PRACTITIONERS:
        repo.execute(
            "INSERT INTO practitioners (id, name, specialty, room_id) VALUES (?, ?, ?, ?)", (pid, name, specialty, room)
        )
        schedules.set_working_days(pid, weekdays)
    for day, name in HOLIDAYS:
        repo.execute("INSERT INTO holidays (day, name) VALUES (?, ?)", (day, name))

    clock.set_now(_at("2026-10-20", "10:00"))
    for p in PATIENTS:
        patients.create(*p)
    patients.deactivate(11)
    users.add("ana.reis", "Ana Reis", "reception")
    users.add("hvaz", "Helena Vaz", "practitioner")
    users.add("admin", "Rui Calado", "admin")
    settings_store.set("reminder_time", "18:30")

    # Two weeks of slots: last week (seen patients) and this week.
    for monday in (date(2026, 10, 26), date(2026, 11, 2)):
        for pid, *_ in PRACTITIONERS:
            schedules.generate_week(pid, monday)

    clock.set_now(_at("2026-10-23", "16:20"))
    past = [
        (1, 1, "2026-10-26T09:00", "flu symptoms", "completed"),
        (4, 1, "2026-10-26T09:30", "check-up", "completed"),
        (3, 2, "2026-10-28T10:00", "blood pressure", "completed"),
        (6, 3, "2026-10-26T11:00", "knee", "completed"),
        (6, 3, "2026-10-29T11:00", "knee", "no-show"),
        (7, 4, "2026-10-27T15:00", "mole check", "completed"),
        (2, 5, "2026-10-29T09:30", "vaccination", "completed"),
        (9, 1, "2026-10-30T14:00", None, "no-show"),
        (10, 2, "2026-10-30T16:00", "back pain", "cancelled"),
    ]
    done = []
    for patient, practitioner, starts, reason, status in past:
        appt = appointments.book(patient, _slot(practitioner, starts), reason)
        if status == "cancelled":
            appointments.cancel(appt, "patient called")
        else:
            appointments.set_outcome(appt, status)
            if status == "completed":
                done.append(appt)

    clock.set_now(_at("2026-10-31", "12:00"))
    invoices = [billing.invoice_for(a) for a in done]
    billing.record_payment(invoices[0], 1350, "card")
    billing.record_payment(invoices[1], 500, "cash")
    billing.record_payment(invoices[2], 4500, "mbway")
    billing.record_payment(invoices[4], 1300, "card")
    outbox.build("2026-10-30T18:30")  # last week's reminders went out

    clock.set_now(_at("2026-10-31", "18:45"))
    upcoming = [
        (1, 1, "2026-11-02T09:00", "follow-up"),
        (5, 1, "2026-11-02T10:30", "allergies"),
        (12, 2, "2026-11-02T09:00", "sports certificate"),
        (6, 3, "2026-11-02T11:00", "knee"),
        (8, 5, "2026-11-03T10:00", "fever"),
        (14, 5, "2026-11-03T10:30", None),
        (7, 4, "2026-11-05T15:30", "results"),
        (4, 1, "2026-11-04T12:00", None),
        (3, 2, "2026-11-04T09:30", "prescription"),
        (13, 6, "2026-11-06T09:00", "diet plan"),
    ]
    for patient, practitioner, starts, reason in upcoming:
        appointments.book(patient, _slot(practitioner, starts), reason)

    clock.set_now(_at("2026-10-28", "11:20"))
    notes.add(1, 1, "Fever for three days. Rest and fluids.")
    notes.add(6, 3, "Knee: exercises 3x a week.")
    clock.set_now(_at("2026-10-29", "09:50"))
    notes.add(2, 5, "Vaccines up to date.")
    notes.add(1, 1, "Better. Review in a week.")
    notes.add(3, 2, "BP 150/90; adjust dose.")
    referrals.create(6, 3, "general")
    clock.set_now(_at("2026-10-30", "17:00"))
    referrals.create(7, 4, "general")
    referrals.create(3, 2, "nutrition")

    clock.set_now(_at("2026-10-30", "12:00"))
    waitlist.add(9, 1, "mornings only")
    clock.set_now(_at("2026-10-30", "15:30"))
    waitlist.add(10, 1)
    waitlist.add(11, 4)
    clock.set_now(_at("2026-10-29", "08:10"))
    waitlist.add(2, 1, "after school")
