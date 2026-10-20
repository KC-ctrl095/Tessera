"""Stage 1: read the trips export (CSV, with a header row)."""

FIELDS = ["trip_id", "started_at", "ended_at", "start_station", "end_station", "bike_id", "member_type", "distance_m"]


def read_trips(path):
    """Every trip row as a dict of strings, in file order."""
    rows = []
    with open(path, encoding="utf-8") as f:
        header = f.readline().rstrip("\n").split(",")
        if header != FIELDS:
            raise ValueError(f"unexpected columns: {header}")
        for line in f:
            values = line.rstrip("\r\n").split(",")
            if len(values) != len(FIELDS):
                continue  # a damaged line
            rows.append(dict(zip(FIELDS, values)))
    return rows
