"""The product catalogue: what we sell, its price, weight and size."""

import logging
import re

from .models import CatalogEntry

log = logging.getLogger(__name__)

_SEPARATORS = re.compile(r"[\s_.]")


def normalize_sku(raw):
    """The canonical form of a SKU as people (and partner feeds) type it.

    Trimmed and upper-cased; every run of spaces, underscores, dots or hyphens
    inside it becomes a single hyphen:

        " ab_100 "  -> "AB-100"
        "cd . 20"   -> "CD-20"
        "ef--7"     -> "EF-7"
    """
    return _SEPARATORS.sub("-", raw.strip().upper())


class Catalog:
    def __init__(self):
        self._entries = {}

    def register(self, entry):
        key = normalize_sku(entry.sku)
        if key in self._entries:
            raise ValueError(f"duplicate SKU {key}")
        entry.sku = key
        self._entries[key] = entry
        log.debug("registered %s (%s) price=%s weight=%dg dims=%s", key, entry.name, entry.price, entry.weight_g, entry.dims_mm)
        return entry

    def add(self, sku, name, price, weight_g, dims_mm, vat_rate=None):
        kwargs = {} if vat_rate is None else {"vat_rate": vat_rate}
        return self.register(CatalogEntry(sku, name, price, weight_g, tuple(dims_mm), **kwargs))

    def get(self, sku):
        key = normalize_sku(sku)
        entry = self._entries.get(key)
        if entry is None:
            log.debug("catalog miss for %r", sku)
        return entry

    def __contains__(self, sku):
        return self.get(sku) is not None

    def __len__(self):
        return len(self._entries)

    def skus(self):
        return sorted(self._entries)

    def price_table(self):
        """{canonical SKU: (unit price, VAT rate, name)} for invoicing."""
        return {k: (e.price, e.vat_rate, e.name) for k, e in self._entries.items()}
