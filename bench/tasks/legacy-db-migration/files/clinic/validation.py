"""Input checks shared by the modules (no database access)."""

import re

from .errors import Invalid

_DAY = re.compile(r"^\d{4}-\d{2}-\d{2}$")


def require_name(name):
    if not name or not name.strip():
        raise Invalid("a name is required")
    if len(name) > 120:
        raise Invalid("that name is too long")
    return name.strip()


def normalise_phone(phone):
    """'+351 912 345 678' -> '+351912345678'; None stays None."""
    if phone is None:
        return None
    digits = re.sub(r"[^\d+]", "", phone)
    if digits and not digits.startswith("+"):
        digits = "+351" + digits
    if not re.fullmatch(r"\+\d{9,15}", digits or ""):
        raise Invalid(f"not a phone number: {phone!r}")
    return digits


def require_day(day):
    if not isinstance(day, str) or not _DAY.match(day):
        raise Invalid(f"not a day (YYYY-MM-DD): {day!r}")
    return day
