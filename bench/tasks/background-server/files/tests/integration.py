"""Integration checks against a running dev server.

    python3 -m parcelhub.serve        # in another terminal; ready after about 15 s
    python3 tests/integration.py      # uses the host and port in config/dev.json

Prints PASS or FAIL per check; exits 0 when all pass.
"""

import json
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import quote

CONFIG = Path(__file__).resolve().parent.parent / "config" / "dev.json"


def base_url():
    cfg = json.loads(CONFIG.read_text(encoding="utf-8"))
    return f"http://{cfg.get('host', '127.0.0.1')}:{cfg['port']}"


BASE = base_url()


def call(method, url, body=None):
    """(status, headers with lower-case names, decoded JSON body)."""
    if url.startswith("/"):
        url = BASE + url
    data = None if body is None else json.dumps(body).encode("utf-8")
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            status, headers, raw = r.status, r.headers, r.read()
    except urllib.error.HTTPError as e:
        status, headers, raw = e.code, e.headers, e.read()
    return status, {k.lower(): v for k, v in headers.items()}, json.loads(raw.decode("utf-8"))


def links(headers):
    """{rel: url} from a Link header."""
    return {rel: url for url, rel in re.findall(r'<([^>]*)>\s*;\s*rel="([^"]+)"', headers.get("link", ""))}


CHECKS = []


def check(fn):
    CHECKS.append(fn)
    return fn


@check
def health():
    status, _, body = call("GET", "/health")
    assert status == 200 and body == {"status": "ok", "parcels": 420}, (status, body)


@check
def get_one_parcel():
    status, _, body = call("GET", "/parcels/PH-100007")
    assert status == 200, status
    assert body["id"] == "PH-100007" and body["events"] and body["events"][0]["status"] == "created", body


@check
def unknown_parcel_is_404():
    status, _, body = call("GET", "/parcels/PH-999999")
    assert status == 404 and "error" in body, (status, body)


@check
def first_page_of_a_filter():
    status, headers, body = call("GET", "/parcels?status=in_transit&per_page=20")
    assert status == 200, (status, body)
    assert headers.get("x-total-count") == "87", f"X-Total-Count is {headers.get('x-total-count')}, expected 87"
    assert len(body["items"]) == 20 and all(p["status"] == "in_transit" for p in body["items"])
    rels = links(headers)
    assert "next" in rels and "prev" not in rels, f"Link rels on the first page: {sorted(rels)}"


@check
def last_page_has_no_next_link():
    status, headers, body = call("GET", "/parcels?status=returned&per_page=10&page=3")
    assert status == 200, (status, body)
    assert len(body["items"]) == 10, len(body["items"])
    rels = links(headers)
    assert "prev" in rels, f"no prev link on page 3: {headers.get('link')}"
    assert "next" not in rels, f"page 3 of 3 links to a next page: {headers.get('link')}"


@check
def city_with_accents_and_paging():
    status, headers, body = call("GET", "/parcels?city=" + quote("Łódź") + "&per_page=5")
    assert status == 200, f"status {status}: {body}"
    assert headers.get("x-total-count") == "12", f"X-Total-Count is {headers.get('x-total-count')}, expected 12"
    first = [p["id"] for p in body["items"]]
    nxt = links(headers).get("next")
    assert nxt, f"no next link: {headers.get('link')}"
    status, headers, body = call("GET", nxt)
    assert status == 200, f"following the next link gave {status}: {body}"
    second = [p["id"] for p in body["items"]]
    assert len(second) == 5 and not set(first) & set(second), (first, second)
    assert all(p["city"] == "Łódź" for p in body["items"]), body["items"]


@check
def page_size_is_limited():
    status, _, body = call("GET", "/parcels?per_page=500")
    assert status == 400 and "per_page" in body.get("error", ""), (status, body)


@check
def add_a_tracking_event():
    status, headers, body = call("POST", "/parcels/PH-100042/events", {"status": "out_for_delivery", "location": "Depot Utrecht"})
    assert status == 201, (status, body)
    status, _, body = call("GET", "/parcels/PH-100042")
    last = body["events"][-1]
    assert (last["status"], last["location"]) == ("out_for_delivery", "Depot Utrecht"), last


def main():
    try:
        call("GET", "/health")
    except (urllib.error.URLError, ConnectionError, OSError) as e:
        print(f"can't reach the dev server at {BASE} ({e}). Is it running? Start it with: python3 -m parcelhub.serve")
        return 2
    failed = 0
    for fn in CHECKS:
        try:
            fn()
            print(f"PASS  {fn.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"FAIL  {fn.__name__}: {e}")
        except Exception as e:  # noqa: BLE001
            failed += 1
            print(f"FAIL  {fn.__name__}: {type(e).__name__}: {e}")
    print(f"{len(CHECKS) - failed}/{len(CHECKS)} checks passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
