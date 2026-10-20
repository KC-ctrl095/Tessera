"""/customers endpoints."""

from ...models import Customer
from ...validation.vat_ids import normalize
from ..serializers import customer_to_dict
from ..store import STORE

KINDS = ("business", "consumer")


def post_customer(body):
    """POST /customers {name, country, kind?, vat_id?, email?}"""
    if not body.get("name") or not body.get("country"):
        return 400, {"error": "name and country are required"}
    kind = body.get("kind", "business")
    if kind not in KINDS:
        return 400, {"error": f"kind must be one of {KINDS}"}
    cid = max(STORE.customers, default=0) + 1
    vat_id = normalize(body["vat_id"]) if body.get("vat_id") else None
    c = Customer(id=cid, name=body["name"], country=body["country"].upper(), kind=kind, vat_id=vat_id, email=body.get("email", ""))
    STORE.customers[cid] = c
    return 201, customer_to_dict(c)


def get_customer(cid):
    c = STORE.customers.get(cid)
    return (200, customer_to_dict(c)) if c else (404, {"error": "no such customer"})
