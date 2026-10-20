"""The database schema."""

import legacy_db

SCHEMA = """
CREATE TABLE rooms (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    floor INTEGER NOT NULL
);
CREATE TABLE practitioners (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    specialty TEXT NOT NULL,
    room_id INTEGER REFERENCES rooms (id),
    active INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE working_days (
    practitioner_id INTEGER NOT NULL REFERENCES practitioners (id),
    weekday INTEGER NOT NULL CHECK (weekday BETWEEN 0 AND 6),
    PRIMARY KEY (practitioner_id, weekday)
);
CREATE TABLE patients (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    birth_date TEXT,
    phone TEXT,
    email TEXT,
    insurer TEXT,
    active INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE slots (
    id INTEGER PRIMARY KEY,
    practitioner_id INTEGER NOT NULL REFERENCES practitioners (id),
    starts_at TEXT NOT NULL,
    minutes INTEGER NOT NULL,
    taken INTEGER NOT NULL DEFAULT 0,
    UNIQUE (practitioner_id, starts_at)
);
CREATE TABLE appointments (
    id INTEGER PRIMARY KEY,
    patient_id INTEGER NOT NULL REFERENCES patients (id),
    practitioner_id INTEGER NOT NULL REFERENCES practitioners (id),
    slot_id INTEGER NOT NULL REFERENCES slots (id),
    reason TEXT,
    status TEXT NOT NULL DEFAULT 'booked',
    created_at TEXT NOT NULL
);
CREATE TABLE reminders (
    id INTEGER PRIMARY KEY,
    appointment_id INTEGER NOT NULL REFERENCES appointments (id),
    send_at TEXT NOT NULL,
    channel TEXT NOT NULL,
    sent INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE waitlist (
    id INTEGER PRIMARY KEY,
    patient_id INTEGER NOT NULL REFERENCES patients (id),
    practitioner_id INTEGER NOT NULL REFERENCES practitioners (id),
    added_at TEXT NOT NULL,
    note TEXT
);
CREATE TABLE notes (
    id INTEGER PRIMARY KEY,
    patient_id INTEGER NOT NULL REFERENCES patients (id),
    practitioner_id INTEGER REFERENCES practitioners (id),
    written_at TEXT NOT NULL,
    body TEXT NOT NULL
);
CREATE TABLE invoices (
    id INTEGER PRIMARY KEY,
    appointment_id INTEGER NOT NULL REFERENCES appointments (id),
    patient_id INTEGER NOT NULL REFERENCES patients (id),
    amount_cents INTEGER NOT NULL,
    issued_on TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open'
);
CREATE TABLE payments (
    id INTEGER PRIMARY KEY,
    invoice_id INTEGER NOT NULL REFERENCES invoices (id),
    amount_cents INTEGER NOT NULL,
    paid_on TEXT NOT NULL,
    method TEXT NOT NULL
);
CREATE TABLE referrals (
    id INTEGER PRIMARY KEY,
    patient_id INTEGER NOT NULL REFERENCES patients (id),
    from_practitioner INTEGER NOT NULL REFERENCES practitioners (id),
    to_specialty TEXT NOT NULL,
    created_on TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open'
);
CREATE TABLE holidays (
    day TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE staff (
    id INTEGER PRIMARY KEY,
    login TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    role TEXT NOT NULL
);
CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY,
    at TEXT NOT NULL,
    action TEXT NOT NULL,
    ref_id INTEGER,
    detail TEXT
);
"""


def create():
    legacy_db.connection().executescript(SCHEMA)
