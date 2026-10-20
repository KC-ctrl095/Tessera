"""Stage 4: add each station's district from the station list."""

import json

UNKNOWN = "Unknown"


def load_stations(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)["stations"]


def _key(name):
    return name


def enrich(trips, stations):
    """Adds start_district / end_district; station names match ignoring case and spacing."""
    by_name = {_key(s["name"]): s for s in stations}
    for t in trips:
        for end in ("start", "end"):
            st = by_name.get(_key(t[f"{end}_station"]))
            t[f"{end}_district"] = st["district"] if st else UNKNOWN
            if st:
                t[f"{end}_station"] = st["name"]
    return trips
