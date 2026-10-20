"""Unit-level checks of the two fixed spots, run in the scratch copy. Skips (doesn't fail)
a check whose function was reshaped; the HTTP checks are what count."""

import json
import sys
from urllib.parse import parse_qs, urlsplit

sys.path.insert(0, ".")
out = {"failures": [], "skipped": []}
try:
    from parcelhub import pagination, urls
except Exception as e:  # noqa: BLE001
    out["failures"].append(f"parcelhub doesn't import: {type(e).__name__}: {e}")
    print(json.dumps(out))
    sys.exit(0)

try:
    for total, page, per, want in [(60, 3, 20, False), (61, 3, 20, True), (0, 1, 20, False), (20, 1, 20, False),
                                   (21, 1, 20, True), (30, 2, 10, True), (30, 4, 10, False), (1, 1, 1, False)]:
        pg = pagination.paginate(list(range(total)), page, per)
        if pg.has_next != want:
            out["failures"].append(f"paginate({total} items, page {page}, {per} per page).has_next is {pg.has_next}, expected {want}")
except (TypeError, AttributeError) as e:
    out["skipped"].append(f"paginate: {e}")

try:
    params = {"city": "Łódź", "q": "van der Berg & Co", "per_page": 5, "page": 2}
    u = urls.build_url("/parcels", params)
    ok = u.isascii() and " " not in u and parse_qs(urlsplit(u).query) == {k: [str(v)] for k, v in params.items()}
    if not ok:
        out["failures"].append(f"build_url doesn't percent-encode: {u!r}")
except (TypeError, AttributeError) as e:
    out["skipped"].append(f"build_url: {e}")
print(json.dumps(out))
