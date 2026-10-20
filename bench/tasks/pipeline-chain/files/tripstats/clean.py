"""Stage 2: parse times and numbers, tidy station names, drop impossible trips."""

from datetime import datetime, timezone

MIN_SECONDS = 60
MAX_SECONDS = 24 * 3600


def parse_time(text):
    """An ISO-8601 time with "Z" or an offset, as an aware UTC datetime."""
    return datetime.fromisoformat(text[:19]).replace(tzinfo=timezone.utc)


def tidy_name(name):
    """Station names as typed by the export: trim and collapse runs of spaces."""
    return " ".join(name.split())


def clean(rows):
    """(trips, dropped) where dropped counts {"too_short": n, "too_long": n}."""
    out, dropped = [], {"too_short": 0, "too_long": 0}
    for r in rows:
        start, end = parse_time(r["started_at"]), parse_time(r["ended_at"])
        seconds = (end - start).total_seconds()
        if seconds < MIN_SECONDS:
            dropped["too_short"] += 1
            continue
        if seconds > MAX_SECONDS:
            dropped["too_long"] += 1
            continue
        out.append({
            "trip_id": r["trip_id"],
            "start": start,
            "seconds": seconds,
            "start_station": tidy_name(r["start_station"]),
            "end_station": tidy_name(r["end_station"]),
            "casual": r["member_type"] == "casual",
            "km": int(r["distance_m"]) / 1000,
        })
    return out, dropped
