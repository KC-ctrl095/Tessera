"""Starting clinicdesk: database, schema, clock and the demo data."""

from datetime import datetime

import legacy_db

from . import clock, schema, seed

DEMO_NOW = datetime(2026, 11, 2, 8, 0)


def setup(path=":memory:", demo=True, now=DEMO_NOW):
    """Opens a fresh database at `path`; with demo=True it gets the demo week."""
    legacy_db.connect(path)
    schema.create()
    clock.set_now(now)
    if demo:
        seed.load()
        clock.set_now(now)
