"""Writes the ordinary modules around ledgerly's core (committed output)."""

from pathlib import Path

ROOT = Path("files/ledgerly")
RESOURCES = {
    "customers": ("customer", ["name", "country", "kind", "vat_id", "email"], "STORE.customers"),
    "products": ("product", ["sku", "name", "unit_price", "vat_category"], "STORE.products"),
    "payments": ("payment", ["invoice_number", "amount", "method", "paid_on"], None),
    "credit_notes": ("credit note", ["invoice_number", "reason", "amount"], None),
    "quotes": ("quote", ["customer_id", "items", "valid_until"], None),
    "suppliers": ("supplier", ["name", "country", "vat_id", "iban"], None),
    "expenses": ("expense", ["supplier_id", "amount", "category", "spent_on"], None),
    "projects": ("project", ["name", "customer_id", "budget"], None),
    "timesheets": ("timesheet", ["project_id", "hours", "worked_on", "note"], None),
    "webhooks": ("webhook", ["url", "events", "secret_name"], None),
    "users": ("user", ["email", "name", "role"], None),
    "attachments": ("attachment", ["invoice_number", "filename", "size"], None),
}

for res, (thing, fields, store) in RESOURCES.items():
    if res in ("customers", "products"):
        continue
    req = fields[:2]
    body = f'''"""/{res} endpoints: list, create, read, delete {thing}s."""

from itertools import count

_items = {{}}
_ids = count(1)
REQUIRED = {req!r}
FIELDS = {fields!r}


def list_{res}(query):
    """GET /{res}?limit=&offset="""
    try:
        limit = min(int(query.get("limit", 50)), 200)
        offset = max(int(query.get("offset", 0)), 0)
    except ValueError:
        return 400, {{"error": "limit and offset must be integers"}}
    items = sorted(_items.values(), key=lambda x: x["id"])
    return 200, {{"items": items[offset:offset + limit], "total": len(items)}}


def create_{res[:-1] if res.endswith("s") else res}(body):
    """POST /{res}"""
    missing = [f for f in REQUIRED if f not in body]
    if missing:
        return 400, {{"error": f"missing: {{', '.join(missing)}}"}}
    item = {{"id": next(_ids), **{{f: body.get(f) for f in FIELDS}}}}
    _items[item["id"]] = item
    return 201, item


def get_{res[:-1] if res.endswith("s") else res}(item_id):
    """GET /{res}/<id>"""
    item = _items.get(item_id)
    return (200, item) if item else (404, {{"error": "no such {thing}"}})


def delete_{res[:-1] if res.endswith("s") else res}(item_id):
    """DELETE /{res}/<id>"""
    return (204, None) if _items.pop(item_id, None) else (404, {{"error": "no such {thing}"}})
'''
    (ROOT / "api" / "handlers" / f"{res}.py").write_text(body)

(ROOT / "api" / "handlers" / "customers.py").write_text('''"""/customers endpoints."""

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
''')
(ROOT / "api" / "handlers" / "products.py").write_text('''"""/products endpoints."""

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
''')

REPORTS = {
    "sales_by_country": ("Net sales per customer country.", "inv.customer.country", "inv.net_total"),
    "vat_summary": ("VAT charged per rate (for the periodic VAT return).", None, None),
    "aging": ("Unpaid invoices by days overdue (0-30, 31-60, 61-90, 90+).", None, None),
    "top_customers": ("Customers by gross revenue.", "inv.customer.name", "inv.gross_total"),
    "monthly_revenue": ("Gross revenue per month (YYYY-MM).", "inv.issued_on.strftime('%Y-%m')", "inv.gross_total"),
    "product_mix": ("Net revenue per product SKU.", None, None),
}
for name, (doc, key, value) in REPORTS.items():
    if key:
        code = f'''"""{doc}"""

from collections import defaultdict
from decimal import Decimal


def build(invoices):
    totals = defaultdict(lambda: Decimal("0.00"))
    for inv in invoices:
        totals[{key}] += {value}
    return sorted(totals.items(), key=lambda kv: (-kv[1], kv[0]))
'''
    elif name == "vat_summary":
        code = f'''"""{doc}"""

from collections import defaultdict
from decimal import Decimal


def build(invoices):
    """[(rate, net, vat)] sorted by rate, highest first."""
    net = defaultdict(lambda: Decimal("0.00"))
    vat = defaultdict(lambda: Decimal("0.00"))
    for inv in invoices:
        for line in inv.lines:
            net[line.vat_rate] += line.net
            vat[line.vat_rate] += line.vat
    return [(r, net[r], vat[r]) for r in sorted(net, reverse=True)]
'''
    elif name == "aging":
        code = f'''"""{doc}"""

BUCKETS = [(30, "0-30"), (60, "31-60"), (90, "61-90")]


def bucket(days_overdue):
    for limit, label in BUCKETS:
        if days_overdue <= limit:
            return label
    return "90+"


def build(invoices, today, paid_numbers=()):
    out = {{label: [] for _, label in BUCKETS}}
    out["90+"] = []
    for inv in invoices:
        if inv.number in paid_numbers or inv.due_on >= today:
            continue
        out[bucket((today - inv.due_on).days)].append(inv.number)
    return out
'''
    else:
        code = f'''"""{doc}"""

from collections import defaultdict
from decimal import Decimal


def build(invoices):
    totals = defaultdict(lambda: Decimal("0.00"))
    for inv in invoices:
        for line in inv.lines:
            totals[line.product.sku] += line.net
    return dict(sorted(totals.items()))
'''
    (ROOT / "reports" / f"{name}.py").write_text(code)
(ROOT / "reports" / "__init__.py").write_text('"""Reports over issued invoices."""\n')

INTEGRATIONS = {
    "email_sender": '''"""Sends invoices by email (the outbox is a folder the mail relay picks up)."""

from pathlib import Path

from ..rendering.text_renderer import render_invoice_text

OUTBOX = Path("outbox")


def queue_invoice_email(invoice, outbox=OUTBOX):
    outbox.mkdir(parents=True, exist_ok=True)
    path = outbox / f"{invoice.number.replace(' ', '_').replace('/', '-')}.eml"
    path.write_text(f"To: {invoice.customer.email}\\nSubject: Invoice {invoice.number}\\n\\n{render_invoice_text(invoice)}")
    return path
''',
    "saft_export": '''"""SAF-T (PT) export lines for the tax authority. Tax codes: NOR standard, INT intermediate,
RED reduced, ISE exempt (with the exemption reason in the notes)."""

CODES = {"standard": "NOR", "intermediate": "INT", "reduced": "RED", "exempt": "ISE"}


def tax_code(line):
    if line.vat_rate == 0:
        return "ISE"
    return CODES[line.product.vat_category]


def export_lines(invoice):
    return [
        {"invoice": invoice.number, "sku": l.product.sku, "tax_code": tax_code(l), "net": f"{l.net:.2f}", "tax": f"{l.vat:.2f}"}
        for l in invoice.lines
    ]
''',
    "accounting_csv": '''"""CSV export for the accountant."""

import csv
import io


def to_csv(invoices):
    buf = io.StringIO()
    w = csv.writer(buf)
    w.writerow(["number", "issued_on", "customer", "country", "net", "vat", "gross"])
    for inv in invoices:
        w.writerow([inv.number, inv.issued_on.isoformat(), inv.customer.name, inv.customer.country,
                    f"{inv.net_total:.2f}", f"{inv.vat_total:.2f}", f"{inv.gross_total:.2f}"])
    return buf.getvalue()
''',
    "bank_feed": '''"""Matches bank transactions to open invoices by reference."""

import re

REF = re.compile(r"FT\\s?(\\d{4})/(\\d{5})")


def match(transactions, invoices):
    by_number = {inv.number: inv for inv in invoices}
    out = []
    for t in transactions:
        m = REF.search(t.get("reference", ""))
        number = f"FT {m.group(1)}/{m.group(2)}" if m else None
        out.append((t, by_number.get(number)))
    return out
''',
    "sftp_upload": '''"""Uploads exports to the accountant's server (here: copies into a drop folder)."""

import shutil
from pathlib import Path


def upload(path, drop=Path("drop")):
    drop.mkdir(parents=True, exist_ok=True)
    return Path(shutil.copy(path, drop / Path(path).name))
''',
    "webhook_dispatch": '''"""Builds webhook payloads for invoice events (delivery is done by the worker)."""

import hashlib
import hmac
import json

EVENTS = ("invoice.created", "invoice.paid", "invoice.cancelled")


def payload(event, invoice_dict, secret):
    if event not in EVENTS:
        raise ValueError(f"unknown event {event}")
    body = json.dumps({"event": event, "invoice": invoice_dict}, sort_keys=True)
    sig = hmac.new(secret.encode(), body.encode(), hashlib.sha256).hexdigest()
    return body, {"X-Ledgerly-Signature": sig}
''',
}
(ROOT / "integrations" / "__init__.py").write_text('"""Connections to other systems."""\n')
for name, code in INTEGRATIONS.items():
    (ROOT / "integrations" / f"{name}.py").write_text(code)

UTILS = {
    "dates": '''"""Date helpers."""

from datetime import date, timedelta


def month_bounds(d: date):
    first = d.replace(day=1)
    nxt = (first + timedelta(days=32)).replace(day=1)
    return first, nxt - timedelta(days=1)


def add_business_days(d: date, n: int) -> date:
    while n > 0:
        d += timedelta(days=1)
        if d.weekday() < 5:
            n -= 1
    return d
''',
    "text": '''"""Text helpers."""

import unicodedata


def slug(s: str) -> str:
    s = unicodedata.normalize("NFKD", s).encode("ascii", "ignore").decode()
    return "-".join("".join(ch if ch.isalnum() else " " for ch in s.lower()).split())


def truncate(s: str, n: int) -> str:
    return s if len(s) <= n else s[: n - 1] + "…"
''',
    "pagination": '''"""limit/offset parsing shared by list endpoints."""


def parse(query, default=50, maximum=200):
    limit = min(int(query.get("limit", default)), maximum)
    offset = max(int(query.get("offset", 0)), 0)
    return limit, offset
''',
    "retry": '''"""Retries with exponential backoff for flaky integrations."""

import time


def retry(fn, attempts=3, base=0.5, sleep=time.sleep):
    for i in range(attempts):
        try:
            return fn()
        except OSError:
            if i == attempts - 1:
                raise
            sleep(base * 2 ** i)
''',
    "csvio": '''"""Reading CSV imports (customers, products)."""

import csv


def read_rows(path):
    with open(path, newline="", encoding="utf-8-sig") as f:
        return [{k.strip().lower(): (v or "").strip() for k, v in row.items()} for row in csv.DictReader(f)]
''',
    "ids": '''"""Short random ids for public links."""

import secrets

ALPHABET = "23456789abcdefghjkmnpqrstuvwxyz"


def short_id(n=10):
    return "".join(secrets.choice(ALPHABET) for _ in range(n))
''',
}
(ROOT / "utils" / "__init__.py").write_text("")
for name, code in UTILS.items():
    (ROOT / "utils" / f"{name}.py").write_text(code)

SERVICES = {
    "credit_notes": '''"""Credit notes cancel all or part of an invoice."""

from decimal import Decimal


def credit_amount(invoice, lines=None):
    """Gross amount credited: the whole invoice, or the given line indexes."""
    chosen = invoice.lines if lines is None else [invoice.lines[i] for i in lines]
    return sum((l.gross for l in chosen), Decimal("0.00"))
''',
    "reminders": '''"""Payment reminders for overdue invoices."""


def reminder_text(invoice, today):
    days = (today - invoice.due_on).days
    if days <= 0:
        return None
    return f"Invoice {invoice.number} is {days} day{'s' if days != 1 else ''} overdue ({invoice.gross_total:.2f} {invoice.currency})."
''',
    "quotes": '''"""Quotes become invoices when accepted."""

from .invoicing import create_invoice


def accept_quote(quote, customer, products, issued_on):
    items = [(products[i["sku"]], i["quantity"]) for i in quote["items"]]
    return create_invoice(customer, items, issued_on)
''',
    "recurring": '''"""Monthly recurring invoices (subscriptions)."""

from datetime import date

from .invoicing import create_invoice


def run_month(subscriptions, year, month):
    issued = date(year, month, 1)
    return [create_invoice(s["customer"], s["items"], issued) for s in subscriptions if s.get("active", True)]
''',
}
for name, code in SERVICES.items():
    (ROOT / "services" / f"{name}.py").write_text(code)
