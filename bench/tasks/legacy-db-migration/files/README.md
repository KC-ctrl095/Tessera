# clinicdesk

The back end of the front-desk app at Clínica da Ribeira: patients,
practitioners, slots and appointments, reminders, the waiting list, notes,
referrals, invoices and payments, plus a few reports and exports. SQLite,
Python 3.10+, standard library only.

- `clinic/`: the application (one module per area; `clinic/api.py` is the JSON API)
- `clinic/app.py`: `setup()` opens the database, creates the schema and loads the demo week
- `db/`: the new database layer, `db.Repo`
- `legacy_db.py`: the old database helper, being replaced (see [MIGRATION.md](MIGRATION.md))

Run the tests with `python3 -m unittest discover -s tests -t .`
