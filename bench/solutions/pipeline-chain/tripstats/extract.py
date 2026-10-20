"""Stage 1: read the trips export (CSV, with a header row)."""

import csv

FIELDS = ["trip_id", "started_at", "ended_at", "start_station", "end_station", "bike_id", "member_type", "distance_m"]


def read_trips(path):
    """Every trip row as a dict of strings, in file order."""
    with open(path, newline="", encoding="utf-8") as f:
        reader = csv.DictReader(f)
        if reader.fieldnames != FIELDS:
            raise ValueError(f"unexpected columns: {reader.fieldnames}")
        return [dict(row) for row in reader]
