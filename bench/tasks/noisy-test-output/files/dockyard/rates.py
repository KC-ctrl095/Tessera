"""Carrier rate cards.

Each carrier prices a parcel by zone and weight band. A band covers parcels up
to and including its limit: with bands 500 / 1000 / 2000 g, a 1000 g parcel
pays the 1000 g price and a 1001 g parcel the 2000 g price. Carriers bill the
chargeable weight: the parcel's real weight or its volumetric weight,
whichever is higher.
"""

import bisect
import logging
from decimal import Decimal

from .units import volumetric_weight_g

log = logging.getLogger(__name__)

# carrier -> zone -> [(band limit in grams, price in EUR without VAT)], limits ascending
CARDS = {
    "PostNL": {
        "domestic": [(500, "4.10"), (1000, "5.25"), (2000, "6.95"), (5000, "7.45"), (10000, "9.80"), (20000, "14.50")],
        "eu1": [(500, "8.20"), (1000, "9.75"), (2000, "11.90"), (5000, "15.40"), (10000, "21.00"), (20000, "29.50")],
    },
    "DHL": {
        "domestic": [(1000, "5.40"), (2000, "6.20"), (5000, "7.90"), (10000, "10.40"), (20000, "13.95")],
        "eu1": [(1000, "9.10"), (2000, "11.20"), (5000, "14.80"), (10000, "19.90"), (20000, "27.40")],
    },
    "DPD": {
        "domestic": [(2000, "6.40"), (5000, "7.10"), (10000, "9.60"), (20000, "13.20")],
        "eu1": [(2000, "10.80"), (5000, "13.90"), (10000, "18.70"), (20000, "26.10")],
    },
}
TRANSIT_DAYS = {
    "PostNL": {"domestic": 1, "eu1": 2},
    "DHL": {"domestic": 1, "eu1": 2},
    "DPD": {"domestic": 2, "eu1": 3},
}


class NoRate(LookupError):
    pass


def zone(origin_country, dest_country):
    return "domestic" if origin_country == dest_country else "eu1"


def chargeable_weight_g(parcel):
    return max(parcel.weight_g, volumetric_weight_g(parcel.box.inner_mm))


def band_price(carrier, zone_name, weight_g):
    """The carrier's price for a parcel of weight_g grams in the zone."""
    card = CARDS[carrier][zone_name]
    limits = [limit for limit, _ in card]
    i = bisect.bisect_right(limits, weight_g)
    if i >= len(card):
        raise NoRate(f"{carrier} doesn't take {weight_g} g parcels ({zone_name})")
    price = Decimal(card[i][1])
    log.debug("rate %s/%s for %d g: band <= %d g, %s EUR", carrier, zone_name, weight_g, card[i][0], price)
    return price


def transit_days(carrier, zone_name):
    return TRANSIT_DAYS[carrier][zone_name]
