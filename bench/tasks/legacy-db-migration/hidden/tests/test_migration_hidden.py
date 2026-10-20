"""Every module after the move to db.Repo: same results as before, including the
paths the project's own tests don't reach, and the former BEGIN/COMMIT blocks
still all-or-nothing."""

import unittest
from datetime import date

from clinic import (
    api,
    app,
    appointments,
    audit,
    billing,
    exports,
    holidays,
    notes,
    outbox,
    patients,
    practitioners,
    referrals,
    reminders,
    reports,
    schedules,
    search,
    settings_store,
    slots,
    stats,
    users,
    waitlist,
)
from clinic.errors import Conflict, Invalid, NotFound


def _legacy_conn():
    # Only used when this file is run against the original code.
    try:
        import legacy_db
    except ImportError:
        return None
    return getattr(legacy_db, "_conn", None)


def q(sql, params=()):
    """Rows of a query as tuples, from the database the app opened."""
    conn = _legacy_conn()
    if conn is not None:
        return [tuple(r) for r in conn.execute(sql, params).fetchall()]
    import db

    return [tuple(r.as_dict().values()) for r in db.repo().all(sql, params)]


def script(sql):
    conn = _legacy_conn()
    if conn is not None:
        conn.executescript(sql)
        return
    import db

    db.repo().script(sql)


def fail_on_audit(action):
    """The audit log refuses one action: the last write of an operation fails."""
    script(
        f"CREATE TRIGGER refuse_audit BEFORE INSERT ON audit_log WHEN NEW.action = '{action}'"
        " BEGIN SELECT RAISE(ABORT, 'audit log unavailable'); END;"
    )


def stop_failing():
    script("DROP TRIGGER refuse_audit;")


def snapshot(*tables):
    return {t: q(f"SELECT * FROM {t} ORDER BY rowid") for t in tables}


class Base(unittest.TestCase):
    def setUp(self):
        app.setup()


class AllOrNothing(Base):
    """The former BEGIN/COMMIT blocks: nothing is kept when their last write fails."""

    def assert_nothing_changed(self, before, tables):
        after = snapshot(*tables)
        for t in tables:
            self.assertEqual(after[t], before[t], f"{t} changed although the operation failed")

    def test_book(self):
        tables = ("appointments", "slots", "reminders", "audit_log")
        slot = slots.free(4, "2026-11-03")[0]["id"]
        before = snapshot(*tables)
        fail_on_audit("appointment.booked")
        with self.assertRaises(Exception):
            appointments.book(10, slot, "rash")
        self.assert_nothing_changed(before, tables)
        stop_failing()
        appt = appointments.book(10, slot, "rash")
        self.assertEqual(appointments.detail(appt)["starts_at"], "2026-11-03T09:00")

    def test_cancel(self):
        tables = ("appointments", "slots", "reminders", "audit_log")
        before = snapshot(*tables)
        fail_on_audit("appointment.cancelled")
        with self.assertRaises(Exception):
            appointments.cancel(11, "travelling")
        self.assert_nothing_changed(before, tables)
        stop_failing()
        appointments.cancel(11, "travelling")
        self.assertEqual(appointments.detail(11)["status"], "cancelled")

    def test_merge(self):
        tables = ("patients", "appointments", "notes", "invoices", "waitlist", "referrals", "audit_log")
        before = snapshot(*tables)
        fail_on_audit("patient.merged")
        with self.assertRaises(Exception):
            patients.merge(1, 13)
        self.assert_nothing_changed(before, tables)
        stop_failing()
        self.assertEqual(patients.merge(1, 13), 1)

    def test_record_payment(self):
        tables = ("payments", "invoices", "audit_log")
        before = snapshot(*tables)
        fail_on_audit("invoice.payment")
        with self.assertRaises(Exception):
            billing.record_payment(2, 400, "cash")
        self.assert_nothing_changed(before, tables)
        stop_failing()
        self.assertEqual(billing.record_payment(2, 400, "cash"), 900)
        self.assertEqual(q("SELECT status FROM invoices WHERE id = 2"), [("paid",)])

    def test_generate_day(self):
        tables = ("slots", "audit_log")
        before = snapshot(*tables)
        fail_on_audit("slots.generated")
        with self.assertRaises(Exception):
            slots.generate_day(6, "2026-11-13")
        self.assert_nothing_changed(before, tables)
        stop_failing()
        self.assertEqual(slots.generate_day(6, "2026-11-13"), 16)

    def test_failed_checks_leave_no_transaction_open(self):
        taken = slots.free(1, "2026-11-02")[0]["id"]
        appointments.book(3, taken)
        for _ in range(2):
            with self.assertRaises(Conflict):
                appointments.book(4, taken)
            with self.assertRaises(NotFound):
                appointments.cancel(999)
            with self.assertRaises(NotFound):
                billing.record_payment(999, 100)
        self.assertEqual(slots.generate_day(6, "2026-11-20"), 16)
        self.assertEqual(appointments.book(4, slots.free(1, "2026-11-02")[0]["id"]) > 0, True)


class PatientsAndSearch(Base):
    def test_not_found(self):
        with self.assertRaises(NotFound):
            patients.update_contact(999, email="x@mail.example")
        self.assertFalse(patients.deactivate(999))

    def test_merge(self):
        with self.assertRaises(Invalid):
            patients.merge(1, 1)
        with self.assertRaises(NotFound):
            patients.merge(1, 999)
        self.assertEqual(patients.merge(1, 13), 1)
        with self.assertRaises(NotFound):
            patients.get(13)
        self.assertEqual(appointments.day_list(6, "2026-11-06")[0]["patient"], "Joana Matos")
        self.assertEqual(q("SELECT patient_id FROM appointments WHERE id = 19"), [(1,)])

    def test_by_phone(self):
        self.assertEqual(search.by_phone("912 400 101"), 1)
        self.assertEqual(search.by_phone("+351 936 220 418"), 4)
        self.assertIsNone(search.by_phone("+351 999 999 999"))

    def test_names_of(self):
        self.assertEqual(search.names_of([13, 2, 999]), [(2, "Tiago Matos"), (13, "Joana Matos")])
        self.assertEqual(search.names_of([5]), [(5, "Leonor Bastos")])
        self.assertEqual(search.names_of([]), [])


class PractitionersAndSlots(Base):
    def test_list_and_get(self):
        self.assertEqual(
            practitioners.list_active(),
            [
                {"id": 5, "name": "Dr. André Seabra", "specialty": "paediatrics", "room_id": 2},
                {"id": 2, "name": "Dr. Nuno Quaresma", "specialty": "general", "room_id": 2},
                {"id": 1, "name": "Dr.ª Helena Vaz", "specialty": "general", "room_id": 1},
                {"id": 4, "name": "Dr.ª Sofia Lacerda", "specialty": "dermatology", "room_id": 3},
                {"id": 6, "name": "Marta Couto", "specialty": "nutrition", "room_id": None},
                {"id": 3, "name": "Rita Paiva", "specialty": "physio", "room_id": 4},
            ],
        )
        self.assertEqual(
            practitioners.get(3), {"id": 3, "name": "Rita Paiva", "specialty": "physio", "room_id": 4, "active": True}
        )
        with self.assertRaises(NotFound):
            practitioners.get(99)

    def test_retire(self):
        self.assertEqual(practitioners.retire(6), 15)
        self.assertFalse(practitioners.get(6)["active"])
        self.assertNotIn(6, [p["id"] for p in practitioners.list_active()])
        self.assertEqual(slots.free(6, "2026-11-06"), [])
        self.assertEqual(len(slots.free(6, "2026-10-30")), 16)

    def test_next_free(self):
        self.assertEqual(slots.next_free(1, "2026-11-02T09:00"), {"id": 258, "starts_at": "2026-11-02T09:30"})
        self.assertEqual(slots.next_free(1, "2026-11-06T16:30"), {"id": 336, "starts_at": "2026-11-06T16:30"})
        self.assertIsNone(slots.next_free(1, "2027-01-01T00:00"))

    def test_count_free(self):
        self.assertEqual(slots.count_free("2026-11-02"), 44)
        self.assertEqual(slots.count_free("2026-11-09"), 0)

    def test_schedules(self):
        self.assertEqual(schedules.working_days(2), [0, 2, 4])
        schedules.set_working_days(6, [4, 0, 0])
        self.assertEqual(schedules.working_days(6), [0, 4])
        self.assertEqual(schedules.generate_week(6, date(2026, 11, 9)), 32)
        with self.assertRaises(Invalid):
            schedules.generate_week(6, date(2026, 11, 10))
        with self.assertRaises(Invalid):
            schedules.set_working_days(6, [7])

    def test_holidays(self):
        self.assertTrue(holidays.is_holiday("2026-12-08"))
        self.assertFalse(holidays.is_holiday("2026-12-09"))
        self.assertEqual(
            holidays.upcoming("2026-11-02"),
            ["2026-12-01 Restauração da Independência", "2026-12-08 Imaculada Conceição", "2026-12-25 Natal"],
        )
        self.assertEqual(holidays.upcoming("2026-12-02", 1), ["2026-12-08 Imaculada Conceição"])


class Appointments(Base):
    def test_detail(self):
        self.assertEqual(
            appointments.detail(10),
            {
                "id": 10,
                "status": "booked",
                "reason": "follow-up",
                "booked_at": "2026-10-31T18:45",
                "slot_id": 257,
                "patient": {"id": 1, "name": "Joana Matos", "phone": "+351912400101", "insurer": "Medis"},
                "practitioner": {"id": 1, "name": "Dr.ª Helena Vaz", "specialty": "general"},
                "starts_at": "2026-11-02T09:00",
                "minutes": 30,
            },
        )
        self.assertEqual(
            appointments.detail(15),
            {
                "id": 15,
                "status": "booked",
                "reason": None,
                "booked_at": "2026-10-31T18:45",
                "slot_id": 468,
                "patient": {"id": 14, "name": "Filipa Antunes", "phone": "+351914909113", "insurer": None},
                "practitioner": {"id": 5, "name": "Dr. André Seabra", "specialty": "paediatrics"},
                "starts_at": "2026-11-03T10:30",
                "minutes": 30,
            },
        )
        with self.assertRaises(NotFound):
            appointments.detail(999)

    def test_outcomes(self):
        appointments.set_outcome(12, "no-show")
        self.assertEqual(appointments.detail(12)["status"], "no-show")
        with self.assertRaises(Conflict):
            appointments.set_outcome(12, "completed")
        with self.assertRaises(ValueError):
            appointments.set_outcome(13, "late")
        with self.assertRaises(NotFound):
            appointments.cancel(999)

    def test_day_list_empty(self):
        self.assertEqual(appointments.day_list(4, "2026-11-04"), [])


class RemindersAndOutbox(Base):
    def test_counts_and_marking(self):
        self.assertEqual(reminders.pending_count(), 17)
        self.assertEqual(reminders.mark_sent([14, 15, 999]), 2)
        self.assertEqual(reminders.mark_sent([14]), 0)
        self.assertEqual(reminders.mark_sent([]), 0)
        self.assertEqual(reminders.pending_count(), 15)

    def test_outbox(self):
        msgs = outbox.build("2026-11-02T18:30")
        self.assertEqual(len(msgs), 11)
        self.assertEqual(
            msgs[2], ("email", "leonor.b@mail.example", "Clínica da Ribeira: reminder for Leonor Bastos, appointment on 2026-11-02 at 10:30.")
        )
        self.assertEqual(outbox.build("2026-11-02T18:30"), [])
        self.assertEqual(reminders.pending_count(), 6)


class Waitlist(Base):
    def test_next_and_remove(self):
        self.assertEqual(waitlist.next_in_line(4), {"id": 3, "patient_id": 11, "added_at": "2026-10-30T15:30"})
        self.assertTrue(waitlist.remove(4))
        self.assertFalse(waitlist.remove(4))
        self.assertEqual(waitlist.next_in_line(1), {"id": 1, "patient_id": 9, "added_at": "2026-10-30T12:00"})

    def test_offer_slot(self):
        slot = slots.free(1, "2026-11-05")[0]["id"]
        appt = waitlist.offer_slot(1, slot)
        d = appointments.detail(appt)
        self.assertEqual((d["patient"]["id"], d["reason"], d["starts_at"]), (2, "from the waiting list", "2026-11-05T09:00"))
        self.assertEqual(waitlist.next_in_line(1)["patient_id"], 9)
        self.assertIsNone(waitlist.offer_slot(5, slots.free(5, "2026-11-05")[0]["id"]))

    def test_duplicate(self):
        with self.assertRaises(Conflict):
            waitlist.add(9, 1)


class Billing(Base):
    def test_errors(self):
        with self.assertRaises(NotFound):
            billing.invoice_for(999)
        with self.assertRaises(Conflict):
            billing.invoice_for(10)
        with self.assertRaises(NotFound):
            billing.record_payment(999, 100)
        with self.assertRaises(ValueError):
            billing.record_payment(2, 0)

    def test_balances(self):
        self.assertEqual({pid: billing.balance(pid) for pid in (1, 2, 4, 6, 7, 13)}, {1: 0, 2: 1200, 4: 400, 6: 1520, 7: 0, 13: 0})
        self.assertEqual(billing.record_payment(4, 520), 520)
        self.assertEqual(billing.balance(6), 1000)
        self.assertEqual(billing.invoice_for(4), 4)


class NotesAndReferrals(Base):
    def test_notes(self):
        self.assertEqual(notes.latest(1), {"id": 4, "written_at": "2026-10-29T09:50", "body": "Better. Review in a week.", "by": 1})
        self.assertIsNone(notes.latest(5))
        self.assertEqual(
            notes.history(1),
            [
                "2026-10-28 Dr.ª Helena Vaz: Fever for three days. Rest and fluids.",
                "2026-10-29 Dr.ª Helena Vaz: Better. Review in a week.",
            ],
        )
        nid = notes.add(5, None, "  Phoned about results. ")
        self.assertEqual(notes.latest(5)["id"], nid)
        self.assertEqual(notes.history(5), ["2026-11-02 unknown: Phoned about results."])
        with self.assertRaises(Invalid):
            notes.add(5, 1, "   ")

    def test_referrals(self):
        self.assertEqual(
            referrals.open_for("general"),
            [{"id": 1, "patient": "Mário Fontes", "since": "2026-10-29"}, {"id": 2, "patient": "Inês Carrilho", "since": "2026-10-30"}],
        )
        self.assertEqual(referrals.open_for("dermatology"), [])
        self.assertEqual(referrals.oldest_open(), (1, "general"))
        script("UPDATE referrals SET status = 'closed';")
        self.assertIsNone(referrals.oldest_open())
        rid = referrals.create(8, 5, "dermatology")
        self.assertEqual(referrals.oldest_open(), (rid, "dermatology"))


class ReportsAndExports(Base):
    def test_reports(self):
        self.assertEqual(
            reports.appointments_per_practitioner("2026-11-02", "2026-11-06"),
            {"Dr. André Seabra": 2, "Dr. Nuno Quaresma": 2, "Dr.ª Helena Vaz": 3, "Dr.ª Sofia Lacerda": 1, "Marta Couto": 1, "Rita Paiva": 1},
        )
        self.assertEqual((reports.no_show_rate(1), reports.no_show_rate(3), reports.no_show_rate(6)), (0.333, 0.5, 0.0))
        self.assertEqual((reports.revenue("2026-10-31", "2026-10-31"), reports.revenue("2026-11-01", "2026-11-30")), (7650, 0))
        self.assertEqual(reports.busiest_day(), ("2026-11-02", 4))

    def test_busiest_day_without_appointments(self):
        app.setup(demo=False)
        self.assertIsNone(reports.busiest_day())
        self.assertEqual(reports.appointments_per_practitioner("2026-11-02", "2026-11-06"), {})

    def test_patients_csv(self):
        csv = exports.patients_csv()
        lines = csv.splitlines()
        self.assertEqual(lines[0], "id,name,birth_date,phone,email,insurer,active")
        self.assertEqual(lines[3], "3,Albertina Rocha,1941-11-02,+351253118220,,,1")
        self.assertEqual(lines[11], "11,Graça Esteves,1949-06-06,+351222405916,,AdvanceCare,0")
        self.assertEqual(len(lines), 15)
        self.assertTrue(csv.endswith("14,Filipa Antunes,2008-03-01,+351914909113,filipa.a@mail.example,,1\n"))

    def test_day_sheet(self):
        self.assertEqual(
            exports.day_sheet("2026-11-03"),
            "Appointments 2026-11-03\n"
            "10:00  Dr. André Seabra       Rafael Simões            fever\n"
            "10:30  Dr. André Seabra       Filipa Antunes",
        )
        self.assertEqual(exports.day_sheet("2026-11-09"), "Appointments 2026-11-09")

    def test_stats(self):
        self.assertEqual(stats.age_groups(date(2026, 11, 2)), {"0-17": 2, "18-39": 5, "40-64": 4, "65+": 2})


class SettingsUsersAudit(Base):
    def test_all_settings(self):
        self.assertEqual(
            settings_store.all_settings(),
            {"clinic_name": "Clínica da Ribeira", "reminder_time": "18:30", "slot_minutes": "30"},
        )
        settings_store.set("clinic_name", "Clínica Nova")
        self.assertEqual(settings_store.all_settings()["clinic_name"], "Clínica Nova")

    def test_users(self):
        self.assertEqual(
            users.list_staff(),
            ["admin / Rui Calado / admin", "hvaz / Helena Vaz / practitioner", "ana.reis / Ana Reis / reception"],
        )
        with self.assertRaises(Conflict):
            users.add("HVaz", "Helena V.", "admin")
        with self.assertRaises(Invalid):
            users.add("zeca", "Zeca", "cleaner")
        self.assertGreater(users.add("zeca", "Zeca Lopes", "reception"), 3)

    def test_audit(self):
        self.assertEqual(audit.count("appointment.booked"), 19)
        self.assertEqual(audit.count("patient.created"), 14)
        self.assertEqual(audit.count("nothing"), 0)
        self.assertEqual(
            audit.recent(2),
            [
                {"at": "2026-10-31T18:45", "action": "appointment.booked", "ref": 19, "detail": None},
                {"at": "2026-10-31T18:45", "action": "appointment.booked", "ref": 18, "detail": None},
            ],
        )


class Api(Base):
    def test_routes(self):
        self.assertEqual(api.handle("GET", "/patients/2")[1]["name"], "Tiago Matos")
        self.assertEqual(api.handle("GET", "/patients/999")[0], 404)
        status, body = api.handle("POST", "/patients", {"name": "Vera Mota", "phone": "+351 913 000 111"})
        self.assertEqual((status, body), (200, {"id": 15}))
        self.assertEqual(api.handle("POST", "/patients", {"name": ""})[0], 400)
        self.assertEqual(api.handle("GET", "/search", {"q": "vera"})[1], {"results": [{"id": 15, "name": "Vera Mota", "phone": "+351913000111"}]})
        free = api.handle("GET", "/practitioners/4/free/2026-11-05")[1]["slots"]
        self.assertEqual(free[0]["starts_at"], "2026-11-05T09:00")
        self.assertEqual(api.handle("POST", "/appointments", {"patient_id": 15, "slot_id": free[0]["id"]})[0], 200)
        self.assertEqual(api.handle("POST", "/appointments", {"patient_id": 15, "slot_id": free[0]["id"]})[0], 409)
        self.assertEqual(api.handle("GET", "/appointments/10")[1]["practitioner"]["name"], "Dr.ª Helena Vaz")
        self.assertEqual(api.handle("GET", "/nowhere")[0], 404)

    def test_cancel_offers_the_slot(self):
        status, body = api.handle("POST", "/appointments/10/cancel", {"reason": "ill"})
        self.assertEqual(status, 200)
        offered = body["offered_to_waitlist"]
        self.assertEqual(appointments.detail(offered)["patient"]["id"], 2)
        self.assertEqual(appointments.detail(offered)["starts_at"], "2026-11-02T09:00")
        self.assertEqual(api.handle("POST", "/appointments/10/cancel", {})[0], 409)

    def test_payment(self):
        self.assertEqual(api.handle("POST", "/invoices/6/payments", {"amount_cents": 1200})[1], {"paid": 1200})
        self.assertEqual(api.handle("POST", "/invoices/99/payments", {"amount_cents": 1200})[0], 404)


if __name__ == "__main__":
    unittest.main()
