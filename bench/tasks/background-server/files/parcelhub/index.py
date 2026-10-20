"""Lookup tables built at start-up.

Building them is quick; what makes start-up slow is the carrier zone cache,
which production fetches from the regional sorting centres (about 15 s). The
dev server simulates that fetch with the same timing, so start-up behaves the
same everywhere.
"""

import time
from collections import defaultdict

from .textutil import fold

ZONE_REGIONS = ("NL", "BE", "DE", "FR", "AT", "CH", "CZ", "SK", "PL", "DK", "SE", "NO", "FI", "IT", "ES")
FETCH_SECONDS_PER_REGION = 1.0


class Index:
    def __init__(self, parcels):
        self.parcels = sorted(parcels, key=lambda p: p.id)
        self.by_id = {p.id: p for p in self.parcels}
        self.by_city = defaultdict(list)
        for p in self.parcels:
            self.by_city[fold(p.city)].append(p)
        self.zones = {}

    def get(self, parcel_id):
        return self.by_id.get(parcel_id)

    def find(self, status=None, city=None, recipient=None, q=None):
        """Parcels matching every given filter, in id order."""
        found = self.by_city.get(fold(city), []) if city else self.parcels
        if status:
            found = [p for p in found if p.status == status]
        if recipient:
            r = fold(recipient)
            found = [p for p in found if r in fold(p.recipient)]
        if q:
            needle = fold(q)
            found = [p for p in found if needle in fold(p.recipient) or needle in fold(p.id) or needle in fold(p.city)]
        return list(found)


def build(parcels, progress=print):
    idx = Index(parcels)
    progress(f"indexed {len(idx.parcels)} parcels in {len(idx.by_city)} cities")
    for n, region in enumerate(ZONE_REGIONS, 1):
        time.sleep(FETCH_SECONDS_PER_REGION)  # simulated fetch from the region's sorting centre
        idx.zones[region] = f"zones-{region.lower()}-2026.09"
        progress(f"zone cache: {n}/{len(ZONE_REGIONS)} regions ({region})")
    return idx
