"""The JSON API of the front-desk app: (method, path, body) -> (status, payload)."""

import re

from . import appointments, billing, patients, search, slots, waitlist
from .errors import Conflict, Invalid, NotFound

_ROUTES = []


def route(method, pattern):
    def wrap(fn):
        _ROUTES.append((method, re.compile("^" + pattern + "$"), fn))
        return fn

    return wrap


@route("GET", r"/patients/(\d+)")
def _patient(body, patient_id):
    return patients.get(int(patient_id))


@route("POST", r"/patients")
def _register(body):
    return {"id": patients.create(body.get("name"), body.get("birth_date"), body.get("phone"), body.get("email"))}


@route("GET", r"/search")
def _search(body):
    return {"results": search.by_name(body.get("q", ""))}


@route("GET", r"/practitioners/(\d+)/free/(\d{4}-\d{2}-\d{2})")
def _free(body, practitioner_id, day):
    return {"slots": slots.free(int(practitioner_id), day)}


@route("POST", r"/appointments")
def _book(body):
    return {"id": appointments.book(body["patient_id"], body["slot_id"], body.get("reason"))}


@route("GET", r"/appointments/(\d+)")
def _appointment(body, appointment_id):
    return appointments.detail(int(appointment_id))


@route("POST", r"/appointments/(\d+)/cancel")
def _cancel(body, appointment_id):
    before = appointments.detail(int(appointment_id))
    appointments.cancel(int(appointment_id), body.get("reason", ""))
    offered = waitlist.offer_slot(before["practitioner"]["id"], before["slot_id"])
    return {"cancelled": True, "offered_to_waitlist": offered}


@route("POST", r"/invoices/(\d+)/payments")
def _pay(body, invoice_id):
    return {"paid": billing.record_payment(int(invoice_id), int(body["amount_cents"]), body.get("method", "card"))}


def handle(method, path, body=None):
    for m, pattern, fn in _ROUTES:
        found = pattern.match(path)
        if m == method and found:
            try:
                return 200, fn(body or {}, *found.groups())
            except NotFound as e:
                return 404, {"error": str(e)}
            except Conflict as e:
                return 409, {"error": str(e)}
            except (Invalid, KeyError, ValueError) as e:
                return 400, {"error": str(e)}
    return 404, {"error": "no such endpoint"}
