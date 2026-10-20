"""Query strings: parsing and the filters /parcels understands."""

from urllib.parse import parse_qs

from .errors import bad_request
from .store import STATUSES

FILTERS = ("status", "city", "recipient", "q")
PAGING = ("page", "per_page")


def parse(query_string):
    """{name: value} from a query string (percent-decoded as UTF-8; the last value of a repeated name wins)."""
    try:
        parsed = parse_qs(query_string, keep_blank_values=True, strict_parsing=False, errors="strict")
    except UnicodeDecodeError:
        raise bad_request("the query string is not valid UTF-8")
    return {k: v[-1] for k, v in parsed.items()}


def filters(params):
    """The filters in the order they were given; unknown names are an error."""
    out = {}
    for name, value in params.items():
        if name in PAGING:
            continue
        if name not in FILTERS:
            raise bad_request(f"unknown filter {name!r} (use one of {', '.join(FILTERS)})")
        if name == "status" and value not in STATUSES:
            raise bad_request(f"unknown status {value!r}")
        if value == "":
            continue
        out[name] = value
    return out
