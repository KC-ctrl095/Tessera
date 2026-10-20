"""limit/offset parsing shared by list endpoints."""


def parse(query, default=50, maximum=200):
    limit = min(int(query.get("limit", default)), maximum)
    offset = max(int(query.get("offset", 0)), 0)
    return limit, offset
