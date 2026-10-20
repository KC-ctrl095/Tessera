"""URL patterns -> handler names."""

import re

ROUTES = [
    ("GET", re.compile(r"^/reports/monthly/(?P<month>\d{4}-\d{2})$"), "monthly_report"),
    ("GET", re.compile(r"^/reports/daily/(?P<month>\d{4}-\d{2})$"), "daily_report"),
    ("GET", re.compile(r"^/payments/(?P<payment_id>\d+)$"), "payment"),
    ("GET", re.compile(r"^/payments/(?P<payment_id>\d+)/invoice$"), "invoice"),
    ("GET", re.compile(r"^/usage$"), "usage"),
]


def match(method: str, path: str):
    for m, pattern, name in ROUTES:
        found = pattern.match(path)
        if m == method and found:
            return name, found.groupdict()
    return None, {}
