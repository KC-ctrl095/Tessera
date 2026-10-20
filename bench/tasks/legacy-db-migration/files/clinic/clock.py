"""The clinic's clock. app.setup() fixes it for the demo data and the tests."""

from datetime import datetime

_fixed = None


def set_now(value):
    """Fix the clock at a datetime, or pass None to follow the real time."""
    global _fixed
    _fixed = value


def now() -> datetime:
    return _fixed if _fixed is not None else datetime.now().replace(microsecond=0)


def now_iso() -> str:
    return now().isoformat(timespec="minutes")


def today():
    return now().date()
