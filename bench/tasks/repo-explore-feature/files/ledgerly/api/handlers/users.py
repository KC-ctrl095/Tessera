"""/users endpoints: list, create, read, delete users."""

from itertools import count

_items = {}
_ids = count(1)
REQUIRED = ['email', 'name']
FIELDS = ['email', 'name', 'role']


def list_users(query):
    """GET /users?limit=&offset="""
    try:
        limit = min(int(query.get("limit", 50)), 200)
        offset = max(int(query.get("offset", 0)), 0)
    except ValueError:
        return 400, {"error": "limit and offset must be integers"}
    items = sorted(_items.values(), key=lambda x: x["id"])
    return 200, {"items": items[offset:offset + limit], "total": len(items)}


def create_user(body):
    """POST /users"""
    missing = [f for f in REQUIRED if f not in body]
    if missing:
        return 400, {"error": f"missing: {', '.join(missing)}"}
    item = {"id": next(_ids), **{f: body.get(f) for f in FIELDS}}
    _items[item["id"]] = item
    return 201, item


def get_user(item_id):
    """GET /users/<id>"""
    item = _items.get(item_id)
    return (200, item) if item else (404, {"error": "no such user"})


def delete_user(item_id):
    """DELETE /users/<id>"""
    return (204, None) if _items.pop(item_id, None) else (404, {"error": "no such user"})
