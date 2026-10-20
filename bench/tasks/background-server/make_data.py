"""Generate files/data/parcels.json for the background-server task (fixed seed, exact counts).

    python3 make_data.py > files/data/parcels.json
"""

import json
import random
from datetime import datetime, timedelta

rng = random.Random(20260917)

STATUS_COUNTS = {"created": 40, "in_transit": 87, "out_for_delivery": 48, "delivered": 190, "returned": 30, "held_at_depot": 25}
CITY_COUNTS = {
    ("Łódź", "PL", "90-001"): 12, ("Plzeň", "CZ", "301 00"): 9, ("Malmö", "SE", "211 20"): 11, ("Zürich", "CH", "8001"): 14,
    ("Kraków", "PL", "30-001"): 16, ("Göteborg", "SE", "411 01"): 10, ("København", "DK", "1050"): 13, ("Košice", "SK", "040 01"): 7,
    ("Århus", "DK", "8000"): 8, ("Utrecht", "NL", "3511"): 58, ("Rotterdam", "NL", "3011"): 45, ("Antwerpen", "BE", "2000"): 35,
    ("Köln", "DE", "50667"): 38, ("Lyon", "FR", "69001"): 30, ("Wien", "AT", "1010"): 32, ("Graz", "AT", "8010"): 20,
    ("Brno", "CZ", "602 00"): 17, ("Gdańsk", "PL", "80-001"): 15, ("Düsseldorf", "DE", "40213"): 20, ("Liège", "BE", "4000"): 10,
}
FIRST = ["Anna", "Bram", "Chloé", "Dawid", "Eva", "Femke", "Gustav", "Hanna", "Ines", "Jakub", "Karin", "Lars", "Marta", "Niels",
         "Olga", "Pieter", "Quinten", "Rosa", "Søren", "Tomasz", "Ula", "Vera", "Wim", "Zofia", "Ömer", "Élodie"]
LAST = ["van der Berg", "de Vries", "Nowak", "Kowalski", "Jansen", "Novák", "Svoboda", "Lindqvist", "Andersen", "Müller", "Schmidt",
        "Peeters", "Dubois", "Gruber", "Huber", "Wiśniewska", "van Dijk", "Bakker", "Nielsen", "Dvořák", "Horváth", "Janssens"]
FLOW = ["created", "in_transit", "out_for_delivery", "delivered"]
DEPOTS = ["Hub Utrecht", "Hub Köln", "Hub Antwerpen", "Hub Praha", "Hub Poznań", "Hub Malmö", "Hub Wien"]

statuses = [s for s, n in STATUS_COUNTS.items() for _ in range(n)]
cities = [c for c, n in CITY_COUNTS.items() for _ in range(n)]
assert len(statuses) == len(cities) == 420
rng.shuffle(statuses)
rng.shuffle(cities)
# The integration test posts an event to PH-100042; it must already be out for delivery.
i42 = 42
j = next(k for k, s in enumerate(statuses) if s == "out_for_delivery")
statuses[i42], statuses[j] = statuses[j], statuses[i42]


def iso(t):
    return t.strftime("%Y-%m-%dT%H:%M:%SZ")


parcels = []
for i in range(420):
    city, country, postcode = cities[i]
    status = statuses[i]
    t = datetime(2026, 9, 1, 6, 0) + timedelta(minutes=rng.randint(0, 29 * 24 * 60))
    events = []
    path = FLOW[: FLOW.index(status) + 1] if status in FLOW else (["created", "in_transit", status] if status == "held_at_depot" else ["created", "in_transit", "out_for_delivery", "returned"])
    for st in path:
        loc = "Shipper" if st == "created" else (city if st in ("out_for_delivery", "delivered") else rng.choice(DEPOTS))
        events.append({"at": iso(t), "status": st, "location": loc})
        t += timedelta(minutes=rng.randint(90, 2000))
    parcels.append({
        "id": f"PH-{100000 + i}",
        "recipient": f"{rng.choice(FIRST)} {rng.choice(LAST)}",
        "city": city,
        "country": country,
        "postcode": postcode,
        "weight_g": rng.randint(80, 24000),
        "status": status,
        "events": events,
    })
# A business customer whose name has an "&" in it (links must encode it).
for i in (12, 57, 101, 150, 199, 233, 301, 377):
    parcels[i]["recipient"] = "Bakker & Zonen BV"
print(json.dumps({"generated": "2026-09-30", "parcels": parcels}, ensure_ascii=False, indent=1))
