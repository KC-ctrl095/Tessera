"""Request handling: (method, path, headers, now) -> (status, body)."""

from billing.invoices import invoice_for
from billing.numbering import Numbering
from core.errors import InvalidRequest, NotFound
from reports.daily import daily_totals
from reports.monthly import monthly_report
from utils.dates import parse_month

from . import serializers
from .auth import customer_for
from .routes import match
from .usage import UsageMeter


class Api:
    def __init__(self, store, meter=None):
        self.store = store
        self.meter = meter or UsageMeter()
        self.numbering = Numbering()

    def handle(self, method: str, path: str, headers: dict, now):
        customer = customer_for(headers)
        if customer is None:
            return 401, {"error": "missing or unknown API key"}
        if not self.meter.allow(customer, now):
            return 429, {"error": "daily quota used up", "resets_at": serializers.local_iso(self.meter.resets_at(now))}
        name, params = match(method, path)
        if name is None:
            return 404, {"error": "no such endpoint"}
        try:
            return 200, getattr(self, "_" + name)(customer=customer, now=now, **params)
        except NotFound as e:
            return 404, {"error": str(e)}
        except (InvalidRequest, ValueError) as e:
            return 400, {"error": str(e)}

    def _monthly_report(self, month, **_):
        year, m = parse_month(month)
        return serializers.report_json(monthly_report(self.store, year, m))

    def _daily_report(self, month, **_):
        year, m = parse_month(month)
        return {"month": month, "days": [{"day": d.isoformat(), "cents": c} for d, c in daily_totals(self.store, year, m)]}

    def _payment(self, payment_id, customer, **_):
        p = self.store.payment(int(payment_id))
        if p.customer_id != customer:
            raise NotFound(f"no payment {payment_id}")
        return serializers.payment_json(p)

    def _invoice(self, payment_id, customer, **_):
        p = self.store.payment(int(payment_id))
        if p.customer_id != customer:
            raise NotFound(f"no payment {payment_id}")
        return serializers.invoice_json(invoice_for(self.store, p, self.numbering))

    def _usage(self, customer, now, **_):
        return {
            "used": self.meter.used(customer, now),
            "remaining": self.meter.remaining(customer, now),
            "resets_at": serializers.local_iso(self.meter.resets_at(now)),
        }
