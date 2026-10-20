"""Generates files/shipping_rules.py (kept for reference; the output is committed)."""

import random

AREAS = [
    "Porto", "Braga", "Aveiro", "Coimbra", "Leiria", "Viseu", "Guarda", "Covilha", "Castelo Branco",
    "Santarem", "Setubal", "Evora", "Beja", "Faro", "Portimao", "Lagos", "Tavira", "Sines",
    "Peniche", "Cascais", "Sintra", "Almada", "Amadora", "Loures", "Barreiro", "Montijo",
    "Palmela", "Seixal", "Mafra", "Torres Vedras", "Lisbon", "Caldas", "Nazare", "Tomar", "Abrantes",
    "Portalegre", "Elvas", "Braganca", "Chaves", "Vila Real",
]
TARGET, TWIN = 31, 12

rng = random.Random(23)
out = ['''"""Shipping prices per zone and service.

Every rule takes the parcel weight in kg and the order total in euros and
returns the shipping price in euros. Orders at or above a zone's free-shipping
threshold ship for free. Up to 2 kg costs the base price; from 2 to 10 kg each
extra kg costs the first rate; above 10 kg each extra kg costs the second rate.

The rules are kept one function per zone and service so that the pricing team
can review changes zone by zone.
"""

# Generated once from the 2025 price sheet, edited by hand since.

''']
names = []
shared_express = None
for z in range(1, len(AREAS) + 1):
    for service in ("economy", "standard", "express"):
        if service == "economy":
            free = rng.choice([40, 50, 60])
            base = rng.choice([2.90, 3.10, 3.40])
            r1 = rng.choice([0.40, 0.50, 0.60])
            r2 = rng.choice([0.80, 0.90, 1.00])
        elif service == "standard":
            free = rng.choice([50, 60, 75, 80])
            base = rng.choice([3.40, 3.90, 4.20, 4.60, 4.90])
            r1 = rng.choice([0.60, 0.70, 0.80, 0.90])
            r2 = rng.choice([1.10, 1.20, 1.35, 1.50])
        else:
            free = rng.choice([100, 120, 120, 150])
            base = rng.choice([8.90, 9.80, 10.50, 11.20])
            r1 = rng.choice([1.10, 1.20, 1.40])
            r2 = rng.choice([4.20, 4.50, 4.50, 4.90])
        if z in (TARGET, TWIN) and service == "express":
            free, base, r1, r2 = 120, 9.80, 1.20, 4.50
        if z in (TARGET - 1, TARGET + 1) and service == "express":
            free, r2 = 120, 4.50
        if z == TARGET and service == "standard":
            free, r2 = 120, 1.50
        name = f"zone_{z:02d}_{service}"
        names.append((z, service, name))
        out.append(f'''def {name}(weight_kg: float, order_total: float) -> float:
    """Zone {z} ({AREAS[z - 1]} area), {service} delivery."""
    if order_total >= {free}:
        return 0.0
    base = {base:.2f}
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * {r1:.2f}, 2)
    return round(base + 8 * {r1:.2f} + (weight_kg - 10) * {r2:.2f}, 2)


''')
out.append("RULES = {\n")
for z, service, name in names:
    out.append(f'    ({z}, "{service}"): {name},\n')
out.append('''}


def price(zone: int, service: str, weight_kg: float, order_total: float) -> float:
    """The shipping price for a parcel; KeyError for an unknown zone or service."""
    if weight_kg <= 0:
        raise ValueError("weight must be positive")
    return RULES[(zone, service)](weight_kg, order_total)
''')
open("files/shipping_rules.py", "w").write("".join(out))
