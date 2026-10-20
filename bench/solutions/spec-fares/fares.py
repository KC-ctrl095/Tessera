"""Fares for river and coastal ferries (see docs/FARES-SPEC.md)."""

from datetime import date, time
from decimal import ROUND_HALF_UP, Decimal

import csv
from pathlib import Path
from decimal import Decimal as D

CSV = Path(__file__).resolve().parent / "docs" / "fares-2026.csv"


def _load_routes():
    routes = {}
    with open(CSV, newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            name, veh, classes = routes.setdefault(row["route"], (row["name"], row["vehicles"] == "yes", {"standard": None, "comfort": None}))
            classes[row["class"]] = {s: D(row[s]) for s in ("low", "high", "peak")}
    return routes


ROUTES = _load_routes()

# Vehicle fares per leg, by season (same on every route that carries vehicles).
VEHICLES = {
    "car": {"low": D("18.00"), "high": D("22.00"), "peak": D("26.00")},
    "motorcycle": {"low": D("7.50"), "high": D("9.00"), "peak": D("10.50")},
}
VAN_PER_METRE = D("3.00")  # per started metre beyond 5.0 m, on top of the car fare
BICYCLE_FARE = D("2.00")  # free in low season

HOLIDAYS_2026 = ["2026-01-01", "2026-02-17", "2026-04-03", "2026-04-05", "2026-04-25", "2026-05-01", "2026-06-04",
                 "2026-06-10", "2026-06-13", "2026-08-15", "2026-10-05", "2026-11-01", "2026-12-01", "2026-12-08"]

# code: (kind, value, min_total, valid_from, valid_to, routes or None, combinable_with_return)
PROMOS = {
    "SUMMER10": ("percent", D("10"), D("0"), "2026-06-15", "2026-09-15", None, True),
    "TROIA5": ("fixed", D("5.00"), D("20.00"), "2026-01-01", "2026-12-31", ["SE-TR", "SE-TR-X"], True),
    "WELCOME": ("percent", D("15"), D("10.00"), "2026-01-01", "2026-12-31", None, False),
    "FAMILY": ("fixed", D("3.00"), D("0"), "2026-01-01", "2026-12-31", None, True),
    "ALGARVE20": ("percent", D("20"), D("15.00"), "2026-05-01", "2026-10-31", ["FA-CU", "FA-FA", "OL-AR", "VR-AY"], False),
    "NIGHTOWL": ("percent", D("25"), D("50.00"), "2026-01-01", "2026-12-31", ["LI-PO"], False),
    "TEJO2": ("fixed", D("2.00"), D("8.00"), "2026-03-01", "2026-11-30", ["CS-BA", "CS-CA", "CS-SE", "CS-MO", "TP-SE", "TP-BA", "BE-PO", "BE-TR"], True),
    "WINTER30": ("percent", D("30"), D("30.00"), "2026-11-01", "2027-02-28", None, False),
    "STUDENT5": ("percent", D("5"), D("0"), "2026-09-01", "2027-06-30", None, True),
    "BIKE": ("fixed", D("1.00"), D("0"), "2026-04-01", "2026-10-31", None, True),
    "MINHO": ("percent", D("12"), D("5.00"), "2026-01-01", "2026-12-31", ["CA-VI"], True),
    "PARTNER45": ("percent", D("45"), D("0"), "2026-01-01", "2026-12-31", None, True),
    "EXPIRED": ("percent", D("50"), D("0"), "2025-01-01", "2025-12-31", None, True),
}

CENT = Decimal("0.01")
ZERO = Decimal("0")


def cents(x):
    return x.quantize(CENT, rounding=ROUND_HALF_UP)


def season(d):
    if d.isoformat() in HOLIDAYS_2026 or (d.month == 12 and d.day >= 24) or (d.month == 1 and d.day == 1):
        return "peak"
    if (6, 15) <= (d.month, d.day) <= (9, 15):
        return "high"
    return "low"


def kind_of(p):
    """The fare category a passenger pays (types corrected by age)."""
    t, age = p["type"], p.get("age")
    if t in ("child", "infant", "adult"):
        if age is None:
            return t
        return "infant" if age <= 3 else "child" if age <= 11 else "adult"
    if t == "senior":
        return "senior" if age is None or age >= 65 else "adult"
    if t == "student":
        return "student" if p.get("card") else "adult"
    return t  # disabled, companion


def passenger_fares(passengers, cls, d, t, adult_fare):
    """Fares of one leg for each passenger (exact, before rounding)."""
    kinds = [kind_of(p) for p in passengers]
    carers = sum(k in ("adult", "senior", "disabled", "student") for k in kinds)
    disabled = kinds.count("disabled")
    weekday = d.weekday() < 5
    peak_hour = weekday and (time(7, 0) <= t < time(9, 30) or time(17, 0) <= t < time(19, 30))
    night = t < time(5, 0)
    adults = kinds.count("adult")
    children = kinds.count("child")
    family_free = 3 if cls == "standard" and adults >= 2 and children >= 2 else 0
    out = []
    free_infants = free_companions = free_children = 0
    for k in kinds:
        extra_infant = False
        if k == "infant":
            if free_infants < carers:
                free_infants += 1
                out.append(ZERO)
                continue
            k, extra_infant = "child", True
        if k == "companion":
            if free_companions < disabled:
                free_companions += 1
                out.append(ZERO)
                continue
            k = "adult"
        if k == "child" and not extra_infant and free_children < family_free:
            free_children += 1
            out.append(ZERO)
            continue
        fare = adult_fare
        if k == "child":
            fare = adult_fare * Decimal("0.5")
        elif k == "senior" and cls == "standard":
            fare = adult_fare * Decimal("0.7")
        elif k == "student" and weekday:
            fare = adult_fare * Decimal("0.75")
        elif k == "disabled":
            fare = adult_fare * Decimal("0.5")
        if peak_hour and cls == "standard" and k in ("adult", "student", "senior"):
            fare *= Decimal("1.15")
        if night:
            fare *= Decimal("0.8")
        out.append(fare)
    return out


def vehicle_fares(vehicles, s):
    out = []
    for v in vehicles:
        t = v["type"]
        if t == "bicycle":
            out.append(ZERO if s == "low" else BICYCLE_FARE)
        elif t == "van":
            extra = max(ZERO, (Decimal(str(v["length_m"])) - 5).to_integral_value(rounding="ROUND_CEILING"))
            out.append(VEHICLES["car"][s] + extra * VAN_PER_METRE)
        elif t in VEHICLES:
            out.append(VEHICLES[t][s])
        else:
            raise ValueError(f"unknown vehicle type {t}")
    return out


def leg(route, cls, d, t, passengers, vehicles):
    name, carries, classes = route
    fares = classes.get(cls)
    if fares is None:
        raise ValueError(f"{cls} class is not offered on this route")
    if vehicles and not carries:
        raise ValueError("this route doesn't carry vehicles")
    s = season(d)
    total = sum(passenger_fares(passengers, cls, d, t, fares[s]), ZERO) + sum(vehicle_fares(vehicles, s), ZERO)
    return s, cents(total)


def quote(booking):
    route = ROUTES.get(booking["route"])
    if route is None:
        raise ValueError(f"unknown route {booking['route']}")
    cls = booking.get("class", "standard")
    passengers, vehicles = booking.get("passengers", []), booking.get("vehicles", [])
    if not passengers:
        raise ValueError("a booking needs at least one passenger")
    out_d = date.fromisoformat(booking["date"])
    out_t = time.fromisoformat(booking["time"])
    legs = [(out_d,) + leg(route, cls, out_d, out_t, passengers, vehicles)]
    ret = booking.get("return")
    if ret:
        rd, rt = date.fromisoformat(ret["date"]), time.fromisoformat(ret["time"])
        if rd < out_d:
            raise ValueError("the return is before the outbound trip")
        legs.append((rd,) + leg(route, cls, rd, rt, passengers, vehicles))
    gross = sum((l[2] for l in legs), ZERO)

    # Group: 10 or more passengers paying a fare on the outbound leg.
    route_fares = route[2][cls][legs[0][1]]
    paying = sum(f > 0 for f in passenger_fares(passengers, cls, out_d, out_t, route_fares))
    after_group = [l[2] - (cents(l[2] * Decimal("0.12")) if paying >= 10 else ZERO) for l in legs]
    group = sum(l[2] for l in legs) - sum(after_group)

    return_disc = ZERO
    if ret and (legs[1][0] - out_d).days <= 30:
        return_disc = cents(after_group[1] * Decimal("0.10"))
    base = sum(after_group) - return_disc

    promo_disc, applied = ZERO, False
    code = booking.get("promo")
    p = PROMOS.get(code) if code else None
    if p:
        kind, value, min_total, start, end, routes, combinable = p
        ok = start <= out_d.isoformat() <= end and (routes is None or booking["route"] in routes)
        base_for_promo = base if combinable else sum(after_group)
        if ok and base_for_promo >= min_total:
            disc = cents(base_for_promo * value / 100) if kind == "percent" else min(value, base_for_promo)
            if combinable or return_disc == 0:
                promo_disc, applied = disc, True
            elif disc > return_disc:
                promo_disc, applied, return_disc = disc, True, ZERO
    discounts = group + return_disc + promo_disc
    cap = cents(gross * Decimal("0.40"))
    if discounts > cap:
        discounts = cap
    fee = Decimal("0.50") if booking.get("channel", "online") == "online" else ZERO
    total = gross - discounts + fee
    if booking.get("payment", "card") == "cash":
        total = (total / Decimal("0.05")).quantize(Decimal("1"), rounding=ROUND_HALF_UP) * Decimal("0.05")
    return {
        "total": f"{cents(total):.2f}",
        "legs": [{"date": l[0].isoformat(), "season": l[1], "subtotal": f"{l[2]:.2f}"} for l in legs],
        "promo_applied": applied,
        "booking_fee": f"{fee:.2f}",
    }
