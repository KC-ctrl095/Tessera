"""Unit conversions.

convert_mass() and volume_cm3() are the API. The old one-purpose helpers
(lbs_to_kg, mm_to_cm) are still used by a few modules and warn when called;
see the 2.0 migration notes in README.md.
"""

import math
import warnings
from decimal import Decimal

_GRAMS = {"g": Decimal(1), "kg": Decimal(1000), "lb": Decimal("453.59237"), "oz": Decimal("28.349523125")}


def convert_mass(value, from_unit, to_unit):
    """Convert a mass between g, kg, lb and oz (Decimal in, Decimal out)."""
    grams = Decimal(value) * _GRAMS[from_unit]
    return grams / _GRAMS[to_unit]


def volume_cm3(dims_mm):
    """Volume of a box given its sides in millimetres, in cubic centimetres."""
    a, b, c = dims_mm
    return a * b * c / 1000.0


def volumetric_weight_g(dims_mm, divisor=5000):
    """What carriers bill a light, bulky parcel at: cm3 / divisor kilograms, in whole grams (rounded up)."""
    return math.ceil(volume_cm3(dims_mm) / divisor * 1000)


def lbs_to_kg(value):
    warnings.warn(
        "dockyard.units.lbs_to_kg is deprecated and will be removed in 3.0; use convert_mass(value, 'lb', 'kg')",
        DeprecationWarning,
        stacklevel=2,
    )
    return convert_mass(value, "lb", "kg")


def mm_to_cm(value):
    warnings.warn(
        "dockyard.units.mm_to_cm is deprecated and will be removed in 3.0; divide by 10 or use volume_cm3()",
        DeprecationWarning,
        stacklevel=2,
    )
    return value / 10.0
