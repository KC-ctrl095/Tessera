"""More HTTP checks against a running server: python3 extra_http.py BASE_URL DATA_JSON

Expected results come from the data file. Prints one line per failure and a
JSON summary as the last line; exits 1 when anything failed.
"""

import json
import math
import re
import sys
import unicodedata
import urllib.error
import urllib.request
from urllib.parse import quote, urlencode

BASE = sys.argv[1].rstrip("/")
with open(sys.argv[2], encoding="utf-8") as f:
    DATA = sorted(json.load(f)["parcels"], key=lambda p: p["id"])
failures = []


def fold(s):
    return "".join(ch for ch in unicodedata.normalize("NFKD", s) if not unicodedata.combining(ch)).casefold()


def expected(status=None, city=None, recipient=None, q=None):
    out = []
    for p in DATA:
        if status and p["status"] != status:
            continue
        if city and fold(p["city"]) != fold(city):
            continue
        if recipient and fold(recipient) not in fold(p["recipient"]):
            continue
        if q and not (fold(q) in fold(p["recipient"]) or fold(q) in fold(p["id"]) or fold(q) in fold(p["city"])):
            continue
        out.append(p["id"])
    return out


def call(url, method="GET", body=None):
    if url.startswith("/"):
        url = BASE + url
    data = None if body is None else json.dumps(body).encode("utf-8")
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            status, headers, raw = r.status, r.headers, r.read()
    except urllib.error.HTTPError as e:
        status, headers, raw = e.code, e.headers, e.read()
    try:
        body = json.loads(raw.decode("utf-8"))
    except ValueError:
        body = raw[:200]
    return status, {k.lower(): v for k, v in headers.items()}, body


def links(headers):
    return {rel: url for url, rel in re.findall(r'<([^>]*)>\s*;\s*rel="([^"]+)"', headers.get("link", ""))}


def walk(name, filters, per_page):
    """Follow the next links from page 1 and compare with the data file."""
    want = expected(**filters)
    url = "/parcels?" + urlencode({**filters, "per_page": per_page})
    ids, pages, prev_url = [], 0, None
    try:
        while url:
            status, headers, body = call(url)
            if status != 200:
                failures.append(f"{name}: GET {url} gave {status}: {body}")
                return
            pages += 1
            ids += [p["id"] for p in body["items"]]
            if headers.get("x-total-count") != str(len(want)):
                failures.append(f"{name}: page {pages} has X-Total-Count {headers.get('x-total-count')}, expected {len(want)}")
                return
            rels = links(headers)
            if pages == 1 and "prev" in rels:
                failures.append(f"{name}: the first page links to a previous page")
                return
            if pages == 2:
                prev_url = rels.get("prev")
            nxt = rels.get("next")
            if nxt is not None and not nxt.isascii():
                failures.append(f"{name}: the next link is not a valid URL (not percent-encoded): {nxt!r}")
                return
            if pages > 60:
                failures.append(f"{name}: next links never end")
                return
            url = nxt
    except Exception as e:  # noqa: BLE001
        failures.append(f"{name}: following the links failed after page {pages}: {type(e).__name__}: {e}")
        return
    want_pages = max(1, math.ceil(len(want) / per_page))
    if ids != want or pages != want_pages:
        failures.append(f"{name}: walking the next links gave {len(ids)} parcels on {pages} page(s), expected {len(want)} on {want_pages}")
        return
    if prev_url:
        status, _, body = call(prev_url)
        if status != 200 or [p["id"] for p in body["items"]] != want[:per_page]:
            failures.append(f"{name}: the prev link of page 2 doesn't lead back to page 1 ({prev_url!r})")


walk("held_at_depot (25, 5 per page)", {"status": "held_at_depot"}, 5)
walk("returned (30, 10 per page)", {"status": "returned"}, 10)
walk("delivered (190, 19 per page)", {"status": "delivered"}, 19)
walk("Plzeň (9, 4 per page)", {"city": "Plzeň"}, 4)
walk("malmö in lower case", {"city": "malmö"}, 3)
walk("Gdańsk (15, 6 per page)", {"city": "Gdańsk"}, 6)
walk("Zürich and delivered", {"city": "Zürich", "status": "delivered"}, 2)
walk("search with a space", {"q": "van der"}, 7)
walk("recipient with ø", {"recipient": "Søren"}, 5)
walk("search with an ampersand", {"q": "Bakker & Zonen"}, 3)
walk("København", {"city": "København"}, 4)

status, headers, body = call("/parcels?city=" + quote("Košice") + "&per_page=20")
if status != 200 or len(body.get("items", [])) != 7 or links(headers):
    failures.append(f"one page of Košice: status {status}, {len(body.get('items', [])) if isinstance(body, dict) else body} items, Link {headers.get('link')!r}")
status, headers, body = call("/parcels?status=returned&per_page=10&page=4")
if status != 200 or body.get("items") != [] or "next" in links(headers):
    failures.append(f"a page past the end: status {status}, Link {headers.get('link')!r}")
for path, why in [("/parcels?per_page=0", "per_page=0"), ("/parcels?page=0", "page=0"), ("/parcels?page=two", "page=two"),
                  ("/parcels?colour=red", "an unknown filter"), ("/parcels?status=lost", "an unknown status")]:
    status, _, body = call(path)
    if status != 400 or not isinstance(body, dict) or "error" not in body:
        failures.append(f"{why} should be a 400 JSON error, got {status}: {body}")
status, _, body = call("/parcels/PH-100300/events", "POST", {"status": "held_at_depot", "location": "Hub Łódź Olechów"})
status2, _, body2 = call("/parcels/PH-100300")
if status != 201 or body2.get("events", [{}])[-1].get("location") != "Hub Łódź Olechów":
    failures.append(f"posting an event with a non-ASCII location: {status} {body}")

for f in failures:
    print("FAIL", f)
print(json.dumps({"failures": len(failures)}))
sys.exit(1 if failures else 0)
