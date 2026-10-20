"""Rough locations from postcodes (the first two digits are enough to pick a warehouse)."""

import logging
import math

log = logging.getLogger(__name__)

# (country, first two digits of the postcode) -> (latitude, longitude) of the area's centre
AREAS = {
    ("NL", "10"): (52.37, 4.90),  # Amsterdam
    ("NL", "25"): (52.08, 4.30),  # The Hague
    ("NL", "30"): (51.92, 4.48),  # Rotterdam
    ("NL", "35"): (52.09, 5.12),  # Utrecht
    ("NL", "56"): (51.44, 5.47),  # Eindhoven
    ("NL", "65"): (51.84, 5.86),  # Nijmegen
    ("NL", "75"): (52.22, 6.89),  # Enschede
    ("NL", "97"): (53.22, 6.57),  # Groningen
    ("BE", "10"): (50.85, 4.35),  # Brussels
    ("BE", "20"): (51.22, 4.40),  # Antwerp
    ("BE", "40"): (50.63, 5.57),  # Liege
    ("BE", "90"): (51.05, 3.72),  # Ghent
    ("DE", "10"): (52.52, 13.40),  # Berlin
    ("DE", "20"): (53.55, 9.99),  # Hamburg
    ("DE", "40"): (51.23, 6.78),  # Duesseldorf
    ("DE", "50"): (50.94, 6.96),  # Cologne
    ("DE", "60"): (50.11, 8.68),  # Frankfurt
    ("DE", "80"): (48.14, 11.58),  # Munich
}


class UnknownArea(LookupError):
    pass


def locate(country, postcode):
    digits = "".join(ch for ch in postcode if ch.isdigit())
    key = (country.upper(), digits[:2])
    if key not in AREAS:
        raise UnknownArea(f"no area for {country} {postcode}")
    return AREAS[key]


def distance_km(a, b):
    """Great-circle distance between two (lat, lon) points."""
    lat1, lon1 = map(math.radians, a)
    lat2, lon2 = map(math.radians, b)
    h = math.sin((lat2 - lat1) / 2) ** 2 + math.cos(lat1) * math.cos(lat2) * math.sin((lon2 - lon1) / 2) ** 2
    return 2 * 6371.0 * math.asin(math.sqrt(h))


def distance_between(country_a, postcode_a, country_b, postcode_b):
    d = distance_km(locate(country_a, postcode_a), locate(country_b, postcode_b))
    log.debug("distance %s %s -> %s %s: %.1f km", country_a, postcode_a, country_b, postcode_b, d)
    return d
