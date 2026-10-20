"""Builds webhook payloads for invoice events (delivery is done by the worker)."""

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
