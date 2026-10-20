"""/credit_notes endpoints: list, create, read, delete credit notes."""

from itertools import count

_items = {}
_ids = count(1)
REQUIRED = ['invoice_number', 'reason']
FIELDS = ['invoice_number', 'reason', 'amount']


def list_credit_notes(query):
    """GET /credit_notes?limit=&offset="""
    try:
        limit = min(int(query.get("limit", 50)), 200)
        offset = max(int(query.get("offset", 0)), 0)
    except ValueError:
        return 400, {"error": "limit and offset must be integers"}
    items = sorted(_items.values(), key=lambda x: x["id"])
    return 200, {"items": items[offset:offset + limit], "total": len(items)}


def create_credit_note(body):
    """POST /credit_notes"""
    missing = [f for f in REQUIRED if f not in body]
    if missing:
        return 400, {"error": f"missing: {', '.join(missing)}"}
    item = {"id": next(_ids), **{f: body.get(f) for f in FIELDS}}
    _items[item["id"]] = item
    return 201, item


def get_credit_note(item_id):
    """GET /credit_notes/<id>"""
    item = _items.get(item_id)
    return (200, item) if item else (404, {"error": "no such credit note"})


def delete_credit_note(item_id):
    """DELETE /credit_notes/<id>"""
    return (204, None) if _items.pop(item_id, None) else (404, {"error": "no such credit note"})
