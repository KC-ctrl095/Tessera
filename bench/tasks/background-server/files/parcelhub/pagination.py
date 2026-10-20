"""Pages of results: ?page=N&per_page=M, an X-Total-Count header and Link headers (RFC 8288)."""

import math
from dataclasses import dataclass
from typing import List

from .errors import bad_request


@dataclass
class Page:
    items: List
    total: int
    page: int
    per_page: int
    last_page: int
    has_prev: bool
    has_next: bool


def _int(value, name):
    try:
        return int(value)
    except (TypeError, ValueError):
        raise bad_request(f"{name} must be a whole number, got {value!r}")


def parse_paging(params, default_size, max_size):
    page = _int(params.get("page", "1"), "page")
    per_page = _int(params.get("per_page", str(default_size)), "per_page")
    if page < 1:
        raise bad_request("page must be 1 or more")
    if not 1 <= per_page <= max_size:
        raise bad_request(f"per_page must be between 1 and {max_size}")
    return page, per_page


def paginate(items, page, per_page):
    total = len(items)
    start = (page - 1) * per_page
    return Page(
        items=items[start : start + per_page],
        total=total,
        page=page,
        per_page=per_page,
        last_page=max(1, math.ceil(total / per_page)),
        has_prev=page > 1,
        has_next=page * per_page <= total,
    )
