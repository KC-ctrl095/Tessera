"""The fare tables, shared by the generator of the spec and the reference solution."""

from decimal import Decimal as D

# code: (name, carries vehicles, {class: {season: adult fare}}); None = class not offered
ROUTES = {
    "CS-BA": ("Cais do Sodré – Barreiro", False, {"standard": {"low": D("2.95"), "high": D("3.40"), "peak": D("3.90")}, "comfort": None}),
    "CS-CA": ("Cais do Sodré – Cacilhas", False, {"standard": {"low": D("1.40"), "high": D("1.65"), "peak": D("1.95")}, "comfort": None}),
    "CS-SE": ("Cais do Sodré – Seixal", False, {"standard": {"low": D("2.55"), "high": D("2.95"), "peak": D("3.35")}, "comfort": {"low": D("4.10"), "high": D("4.75"), "peak": D("5.40")}}),
    "CS-MO": ("Cais do Sodré – Montijo", False, {"standard": {"low": D("3.20"), "high": D("3.70"), "peak": D("4.20")}, "comfort": {"low": D("5.15"), "high": D("5.95"), "peak": D("6.70")}}),
    "BE-PO": ("Belém – Porto Brandão", True, {"standard": {"low": D("1.45"), "high": D("1.70"), "peak": D("1.95")}, "comfort": None}),
    "BE-TR": ("Belém – Trafaria", True, {"standard": {"low": D("1.45"), "high": D("1.70"), "peak": D("1.95")}, "comfort": None}),
    "TP-SE": ("Terreiro do Paço – Seixal", False, {"standard": {"low": D("2.60"), "high": D("3.00"), "peak": D("3.45")}, "comfort": {"low": D("4.20"), "high": D("4.85"), "peak": D("5.55")}}),
    "TP-BA": ("Terreiro do Paço – Barreiro", False, {"standard": {"low": D("2.95"), "high": D("3.40"), "peak": D("3.90")}, "comfort": {"low": D("4.75"), "high": D("5.45"), "peak": D("6.25")}}),
    "SE-TR": ("Setúbal – Tróia", True, {"standard": {"low": D("4.25"), "high": D("6.80"), "peak": D("8.10")}, "comfort": {"low": D("7.20"), "high": D("10.90"), "peak": D("12.60")}}),
    "SE-TR-X": ("Setúbal – Tróia (fast catamaran)", False, {"standard": {"low": D("5.90"), "high": D("8.40"), "peak": D("9.80")}, "comfort": {"low": D("9.10"), "high": D("12.80"), "peak": D("14.90")}}),
    "LI-PO": ("Lisboa – Porto Santo (night)", True, {"standard": {"low": D("38.00"), "high": D("52.00"), "peak": D("61.00")}, "comfort": {"low": D("64.00"), "high": D("88.00"), "peak": D("102.00")}}),
    "FA-CU": ("Faro – Culatra", False, {"standard": {"low": D("2.10"), "high": D("2.40"), "peak": D("2.80")}, "comfort": None}),
    "FA-FA": ("Faro – Farol", False, {"standard": {"low": D("2.10"), "high": D("2.40"), "peak": D("2.80")}, "comfort": None}),
    "OL-AR": ("Olhão – Armona", False, {"standard": {"low": D("1.95"), "high": D("2.25"), "peak": D("2.60")}, "comfort": None}),
    "VR-AY": ("Vila Real de S. António – Ayamonte", True, {"standard": {"low": D("2.00"), "high": D("2.30"), "peak": D("2.70")}, "comfort": {"low": D("3.40"), "high": D("3.90"), "peak": D("4.50")}}),
    "CA-VI": ("Caminha – A Guarda", True, {"standard": {"low": D("1.60"), "high": D("1.85"), "peak": D("2.15")}, "comfort": None}),
}

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
