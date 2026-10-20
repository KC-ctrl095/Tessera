"""The parcels: loaded from the data file, kept in memory."""

import json
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import List

STATUSES = ("created", "in_transit", "out_for_delivery", "delivered", "returned", "held_at_depot")


@dataclass
class Parcel:
    id: str
    recipient: str
    city: str
    country: str
    postcode: str
    weight_g: int
    status: str
    events: List[dict] = field(default_factory=list)


def load(path):
    with open(path, encoding="utf-8") as f:
        raw = json.load(f)
    return [Parcel(**p) for p in raw["parcels"]]


def add_event(parcel, status, location, at=None):
    if status not in STATUSES:
        raise ValueError(f"unknown status {status!r}")
    event = {
        "at": (at or datetime.now(timezone.utc)).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "status": status,
        "location": location,
    }
    parcel.events.append(event)
    parcel.status = status
    return event
