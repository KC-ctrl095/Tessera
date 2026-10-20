"""/timesheets endpoints: list, create, read, delete timesheets."""

from itertools import count

_items = {}
_ids = count(1)
REQUIRED = ['project_id', 'hours']
FIELDS = ['project_id', 'hours', 'worked_on', 'note']


def list_timesheets(query):
    """GET /timesheets?limit=&offset="""
    try:
        limit = min(int(query.get("limit", 50)), 200)
        offset = max(int(query.get("offset", 0)), 0)
    except ValueError:
        return 400, {"error": "limit and offset must be integers"}
    items = sorted(_items.values(), key=lambda x: x["id"])
    return 200, {"items": items[offset:offset + limit], "total": len(items)}


def create_timesheet(body):
    """POST /timesheets"""
    missing = [f for f in REQUIRED if f not in body]
    if missing:
        return 400, {"error": f"missing: {', '.join(missing)}"}
    item = {"id": next(_ids), **{f: body.get(f) for f in FIELDS}}
    _items[item["id"]] = item
    return 201, item


def get_timesheet(item_id):
    """GET /timesheets/<id>"""
    item = _items.get(item_id)
    return (200, item) if item else (404, {"error": "no such timesheet"})


def delete_timesheet(item_id):
    """DELETE /timesheets/<id>"""
    return (204, None) if _items.pop(item_id, None) else (404, {"error": "no such timesheet"})
