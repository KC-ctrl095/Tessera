"""/products endpoints."""

from decimal import Decimal, InvalidOperation

from ...models import Product
from ..store import STORE

CATEGORIES = ("standard", "intermediate", "reduced", "exempt")


def post_product(body):
    """POST /products {sku, name, unit_price, vat_category?}"""
    try:
        price = Decimal(str(body["unit_price"]))
    except (KeyError, InvalidOperation):
        return 400, {"error": "unit_price is required and must be a number"}
    category = body.get("vat_category", "standard")
    if category not in CATEGORIES or not body.get("sku") or not body.get("name"):
        return 400, {"error": "sku, name and a valid vat_category are required"}
    p = Product(sku=body["sku"], name=body["name"], unit_price=price, vat_category=category)
    STORE.products[p.sku] = p
    return 201, {"sku": p.sku, "name": p.name, "unit_price": f"{p.unit_price:.2f}", "vat_category": p.vat_category}
