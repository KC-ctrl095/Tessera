"""Company settings (config/company.json, or the file named by LEDGERLY_CONFIG)."""

import json
import os
from dataclasses import dataclass
from pathlib import Path

DEFAULT_PATH = Path(__file__).resolve().parent.parent / "config" / "company.json"


@dataclass(frozen=True)
class Company:
    name: str
    country: str
    vat_id: str
    currency: str
    address: str
    payment_terms_days: int


_cached = {}


def company(path=None) -> Company:
    """The issuing company's settings (read once per path)."""
    path = Path(path or os.environ.get("LEDGERLY_CONFIG") or DEFAULT_PATH)
    if path not in _cached:
        with open(path, encoding="utf-8") as f:
            _cached[path] = Company(**json.load(f))
    return _cached[path]


def reset():
    """Forget cached settings (tests switch config files)."""
    _cached.clear()
