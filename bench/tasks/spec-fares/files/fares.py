"""Fares for river and coastal ferries.

docs/FARES-SPEC.md is the specification; docs/fares-2026.csv is the fare table.
"""


def quote(booking: dict) -> dict:
    """The price of a booking (spec §10)."""
    raise NotImplementedError("2026 fares not implemented yet")
