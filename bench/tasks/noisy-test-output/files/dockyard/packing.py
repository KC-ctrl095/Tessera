"""Putting order items into boxes."""

import logging

from . import units
from .models import Box, Parcel

log = logging.getLogger(__name__)

BOXES = (
    Box("S", (250, 180, 100), 2000, 120),
    Box("M", (350, 250, 150), 5000, 210),
    Box("L", (500, 350, 250), 12000, 380),
    Box("XL", (600, 400, 400), 20000, 520),
)
FILL = 0.85  # boxes are never filled to the last cubic centimetre


class DoesNotFit(Exception):
    pass


def fits(box, dims_mm):
    """True when an item fits the box in some orientation."""
    return all(a <= b for a, b in zip(sorted(dims_mm), sorted(box.inner_mm)))


def box_cm3(box):
    return units.volume_cm3(box.inner_mm)


def smallest_box_for(dims_mm, weight_g):
    for box in BOXES:
        if fits(box, dims_mm) and box.tare_g + weight_g <= box.max_weight_g:
            return box
    raise DoesNotFit(f"no box for an item of {dims_mm} mm, {weight_g} g")


def pack(items):
    """items: [(sku, catalog entry)] one per unit. Returns parcels, largest items first."""
    units_sorted = sorted(items, key=lambda it: (-units.volume_cm3(it[1].dims_mm), it[0]))
    parcels = []
    for sku, entry in units_sorted:
        cm3 = units.volume_cm3(entry.dims_mm)
        for p in parcels:
            room = box_cm3(p.box) * FILL - p.content_cm3
            if fits(p.box, entry.dims_mm) and cm3 <= room and p.weight_g + entry.weight_g <= p.box.max_weight_g:
                p.skus.append(sku)
                p.content_cm3 += cm3
                p.content_g += entry.weight_g
                log.debug("packed %s into existing %s box (%.0f cm3 left)", sku, p.box.code, room - cm3)
                break
        else:
            box = smallest_box_for(entry.dims_mm, entry.weight_g)
            parcels.append(Parcel(box, [sku], entry.weight_g, cm3))
            log.debug("opened a %s box for %s (longest side %.1f cm)", box.code, sku, units.mm_to_cm(max(box.inner_mm)))
    for i, p in enumerate(parcels, 1):
        log.debug("parcel %d/%d: box %s, %d items, %d g", i, len(parcels), p.box.code, len(p.skus), p.weight_g)
    return parcels
