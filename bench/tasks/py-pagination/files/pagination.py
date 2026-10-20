"""Helpers for paging through result lists in the admin API."""

from dataclasses import dataclass
from math import ceil


@dataclass
class Page:
    items: list
    page: int
    per_page: int
    total: int

    @property
    def pages(self) -> int:
        """Number of pages (an empty result still has one, empty, page)."""
        return max(1, ceil(self.total / self.per_page))

    @property
    def has_next(self) -> bool:
        return self.page <= self.pages

    @property
    def has_prev(self) -> bool:
        return self.page > 1


def paginate(items, page=1, per_page=20):
    """Return the given 1-based page of `items`.

    Pages past the end are empty. A page or per_page below 1 raises ValueError.
    """
    if page < 1 or per_page < 1:
        raise ValueError("page and per_page must be at least 1")
    start = page * per_page
    end = start + per_page
    return Page(list(items[start:end]), page, per_page, len(items))
