"""/webhooks endpoints: list, create, read, delete webhooks."""

from itertools import count

_items = {}
_ids = count(1)
REQUIRED = ['url', 'events']
FIELDS = ['url', 'events', 'secret_name']


def list_webhooks(query):
    """GET /webhooks?limit=&offset="""
    try:
        limit = min(int(query.get("limit", 50)), 200)
        offset = max(int(query.get("offset", 0)), 0)
    except ValueError:
        return 400, {"error": "limit and offset must be integers"}
    items = sorted(_items.values(), key=lambda x: x["id"])
    return 200, {"items": items[offset:offset + limit], "total": len(items)}


def create_webhook(body):
    """POST /webhooks"""
    missing = [f for f in REQUIRED if f not in body]
    if missing:
        return 400, {"error": f"missing: {', '.join(missing)}"}
    item = {"id": next(_ids), **{f: body.get(f) for f in FIELDS}}
    _items[item["id"]] = item
    return 201, item


def get_webhook(item_id):
    """GET /webhooks/<id>"""
    item = _items.get(item_id)
    return (200, item) if item else (404, {"error": "no such webhook"})


def delete_webhook(item_id):
    """DELETE /webhooks/<id>"""
    return (204, None) if _items.pop(item_id, None) else (404, {"error": "no such webhook"})
