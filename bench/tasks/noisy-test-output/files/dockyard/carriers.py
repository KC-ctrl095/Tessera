"""Choosing a carrier for each parcel.

Every carrier's live price API is asked first; when it doesn't answer, the
rate card in rates.py is used (the API and the card agree, the API is just
the source of truth). Express orders need next-day delivery; standard ones
take the cheapest quote that arrives within three days.
"""

import hashlib
import logging

from . import rates
from .models import Quote

log = logging.getLogger(__name__)

SLA_DAYS = {"standard": 3, "express": 1}
FLAKY_CARRIERS = {"DPD"}  # its test endpoint times out now and then


class CarrierTimeout(Exception):
    pass


def _live_price(carrier, zone_name, weight_g, ref):
    """Stand-in for the carriers' HTTP APIs: deterministic, and DPD's sometimes times out."""
    if carrier in FLAKY_CARRIERS and int(hashlib.sha1(ref.encode()).hexdigest(), 16) % 7 == 0:
        raise CarrierTimeout(f"{carrier} price API did not answer within 2.0 s (ref {ref})")
    return rates.band_price(carrier, zone_name, weight_g)


def quote(carrier, parcel, origin_country, dest_country, ref):
    z = rates.zone(origin_country, dest_country)
    weight = rates.chargeable_weight_g(parcel)
    try:
        price = _live_price(carrier, z, weight, ref)
    except CarrierTimeout:
        log.exception("carrier API failed for %s, falling back to the rate card", carrier)
        price = rates.band_price(carrier, z, weight)
    except rates.NoRate as e:
        log.debug("%s: %s", carrier, e)
        return None
    q = Quote(carrier, price, rates.transit_days(carrier, z))
    log.debug("quote %s: %s EUR, %d day(s), %s, %d g chargeable (ref %s)", carrier, q.price, q.days, z, weight, ref)
    return q


def choose(parcel, origin_country, dest_country, service, ref):
    """The cheapest quote within the service's delivery time (ties: faster, then by name)."""
    quotes = [q for q in (quote(c, parcel, origin_country, dest_country, ref) for c in sorted(rates.CARDS)) if q]
    ok = [q for q in quotes if q.days <= SLA_DAYS[service]]
    if not ok:
        log.warning("no carrier meets the %s SLA for %s; quotes were %s", service, ref, [(q.carrier, q.days) for q in quotes])
        return None
    best = min(ok, key=lambda q: (q.price, q.days, q.carrier))
    log.debug("chose %s for %s (%s EUR)", best.carrier, ref, best.price)
    return best
