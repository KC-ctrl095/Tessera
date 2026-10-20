"""The API's endpoints. Each returns (status, JSON payload, extra headers)."""

import re

from . import pagination, query, serialize, store, urls
from .errors import bad_request, not_found

PARCEL = re.compile(r"^/parcels/(PH-\d+)$")
EVENTS = re.compile(r"^/parcels/(PH-\d+)/events$")


def health(ctx, params, body):
    return 200, {"status": "ok", "parcels": len(ctx.index.parcels)}, {}


def list_parcels(ctx, params, body):
    filters = query.filters(params)
    page, per_page = pagination.parse_paging(params, ctx.config.page_size, ctx.config.max_page_size)
    pg = pagination.paginate(ctx.index.find(**filters), page, per_page)
    links = []
    if pg.has_prev:
        links.append((urls.build_url("/parcels", {**filters, "per_page": per_page, "page": page - 1}), "prev"))
    if pg.has_next:
        links.append((urls.build_url("/parcels", {**filters, "per_page": per_page, "page": page + 1}), "next"))
    headers = {"X-Total-Count": str(pg.total)}
    if links:
        headers["Link"] = urls.link_header(ctx.base_url, links)
    payload = {"items": [serialize.summary(p) for p in pg.items], "page": pg.page, "per_page": pg.per_page, "total": pg.total}
    return 200, payload, headers


def get_parcel(ctx, parcel_id):
    p = ctx.index.get(parcel_id)
    if p is None:
        raise not_found(f"no parcel {parcel_id}")
    return 200, serialize.detail(p), {}


def add_event(ctx, parcel_id, body):
    p = ctx.index.get(parcel_id)
    if p is None:
        raise not_found(f"no parcel {parcel_id}")
    if not isinstance(body, dict) or not isinstance(body.get("status"), str) or not isinstance(body.get("location", ""), str):
        raise bad_request('send {"status": "...", "location": "..."}')
    try:
        event = store.add_event(p, body["status"], body.get("location", ""))
    except ValueError as e:
        raise bad_request(str(e))
    return 201, event, {"Location": f"/parcels/{parcel_id}"}


def route(ctx, method, path, params, body):
    if path == "/health" and method == "GET":
        return health(ctx, params, body)
    if path == "/parcels" and method == "GET":
        return list_parcels(ctx, params, body)
    m = PARCEL.match(path)
    if m and method == "GET":
        return get_parcel(ctx, m.group(1))
    m = EVENTS.match(path)
    if m and method == "POST":
        return add_event(ctx, m.group(1), body)
    raise not_found(f"no route for {method} {path}")
