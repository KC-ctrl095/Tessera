"""Shipping prices per zone and service.

Every rule takes the parcel weight in kg and the order total in euros and
returns the shipping price in euros. Orders at or above a zone's free-shipping
threshold ship for free. Up to 2 kg costs the base price; from 2 to 10 kg each
extra kg costs the first rate; above 10 kg each extra kg costs the second rate.

The rules are kept one function per zone and service so that the pricing team
can review changes zone by zone.
"""

# Generated once from the 2025 price sheet, edited by hand since.

def zone_01_economy(weight_kg: float, order_total: float) -> float:
    """Zone 1 (Porto area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 1.00, 2)


def zone_01_standard(weight_kg: float, order_total: float) -> float:
    """Zone 1 (Porto area), standard delivery."""
    if order_total >= 75:
        return 0.0
    base = 4.60
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.35, 2)


def zone_01_express(weight_kg: float, order_total: float) -> float:
    """Zone 1 (Porto area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 9.80
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.90, 2)


def zone_02_economy(weight_kg: float, order_total: float) -> float:
    """Zone 2 (Braga area), economy delivery."""
    if order_total >= 40:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 0.90, 2)


def zone_02_standard(weight_kg: float, order_total: float) -> float:
    """Zone 2 (Braga area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 1.50, 2)


def zone_02_express(weight_kg: float, order_total: float) -> float:
    """Zone 2 (Braga area), express delivery."""
    if order_total >= 150:
        return 0.0
    base = 8.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.90, 2)


def zone_03_economy(weight_kg: float, order_total: float) -> float:
    """Zone 3 (Aveiro area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 0.80, 2)


def zone_03_standard(weight_kg: float, order_total: float) -> float:
    """Zone 3 (Aveiro area), standard delivery."""
    if order_total >= 75:
        return 0.0
    base = 3.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.35, 2)


def zone_03_express(weight_kg: float, order_total: float) -> float:
    """Zone 3 (Aveiro area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 10.50
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.50, 2)


def zone_04_economy(weight_kg: float, order_total: float) -> float:
    """Zone 4 (Coimbra area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.50, 2)
    return round(base + 8 * 0.50 + (weight_kg - 10) * 0.80, 2)


def zone_04_standard(weight_kg: float, order_total: float) -> float:
    """Zone 4 (Coimbra area), standard delivery."""
    if order_total >= 80:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.20, 2)


def zone_04_express(weight_kg: float, order_total: float) -> float:
    """Zone 4 (Coimbra area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 9.80
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.50, 2)


def zone_05_economy(weight_kg: float, order_total: float) -> float:
    """Zone 5 (Leiria area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 1.00, 2)


def zone_05_standard(weight_kg: float, order_total: float) -> float:
    """Zone 5 (Leiria area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 1.50, 2)


def zone_05_express(weight_kg: float, order_total: float) -> float:
    """Zone 5 (Leiria area), express delivery."""
    if order_total >= 100:
        return 0.0
    base = 10.50
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.50, 2)


def zone_06_economy(weight_kg: float, order_total: float) -> float:
    """Zone 6 (Viseu area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 0.90, 2)


def zone_06_standard(weight_kg: float, order_total: float) -> float:
    """Zone 6 (Viseu area), standard delivery."""
    if order_total >= 75:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.80, 2)
    return round(base + 8 * 0.80 + (weight_kg - 10) * 1.50, 2)


def zone_06_express(weight_kg: float, order_total: float) -> float:
    """Zone 6 (Viseu area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 10.50
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.10, 2)
    return round(base + 8 * 1.10 + (weight_kg - 10) * 4.20, 2)


def zone_07_economy(weight_kg: float, order_total: float) -> float:
    """Zone 7 (Guarda area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 0.90, 2)


def zone_07_standard(weight_kg: float, order_total: float) -> float:
    """Zone 7 (Guarda area), standard delivery."""
    if order_total >= 60:
        return 0.0
    base = 4.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.10, 2)


def zone_07_express(weight_kg: float, order_total: float) -> float:
    """Zone 7 (Guarda area), express delivery."""
    if order_total >= 150:
        return 0.0
    base = 9.80
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.20, 2)


def zone_08_economy(weight_kg: float, order_total: float) -> float:
    """Zone 8 (Covilha area), economy delivery."""
    if order_total >= 40:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.50, 2)
    return round(base + 8 * 0.50 + (weight_kg - 10) * 0.90, 2)


def zone_08_standard(weight_kg: float, order_total: float) -> float:
    """Zone 8 (Covilha area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 3.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.10, 2)


def zone_08_express(weight_kg: float, order_total: float) -> float:
    """Zone 8 (Covilha area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 8.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.90, 2)


def zone_09_economy(weight_kg: float, order_total: float) -> float:
    """Zone 9 (Castelo Branco area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.50, 2)
    return round(base + 8 * 0.50 + (weight_kg - 10) * 0.80, 2)


def zone_09_standard(weight_kg: float, order_total: float) -> float:
    """Zone 9 (Castelo Branco area), standard delivery."""
    if order_total >= 80:
        return 0.0
    base = 4.60
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.10, 2)


def zone_09_express(weight_kg: float, order_total: float) -> float:
    """Zone 9 (Castelo Branco area), express delivery."""
    if order_total >= 100:
        return 0.0
    base = 10.50
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.50, 2)


def zone_10_economy(weight_kg: float, order_total: float) -> float:
    """Zone 10 (Santarem area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 0.80, 2)


def zone_10_standard(weight_kg: float, order_total: float) -> float:
    """Zone 10 (Santarem area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.20, 2)


def zone_10_express(weight_kg: float, order_total: float) -> float:
    """Zone 10 (Santarem area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 8.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.50, 2)


def zone_11_economy(weight_kg: float, order_total: float) -> float:
    """Zone 11 (Setubal area), economy delivery."""
    if order_total >= 40:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.50, 2)
    return round(base + 8 * 0.50 + (weight_kg - 10) * 1.00, 2)


def zone_11_standard(weight_kg: float, order_total: float) -> float:
    """Zone 11 (Setubal area), standard delivery."""
    if order_total >= 80:
        return 0.0
    base = 3.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.10, 2)


def zone_11_express(weight_kg: float, order_total: float) -> float:
    """Zone 11 (Setubal area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 10.50
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.90, 2)


def zone_12_economy(weight_kg: float, order_total: float) -> float:
    """Zone 12 (Evora area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 1.00, 2)


def zone_12_standard(weight_kg: float, order_total: float) -> float:
    """Zone 12 (Evora area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 4.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 1.10, 2)


def zone_12_express(weight_kg: float, order_total: float) -> float:
    """Zone 12 (Evora area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 9.80
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.50, 2)


def zone_13_economy(weight_kg: float, order_total: float) -> float:
    """Zone 13 (Beja area), economy delivery."""
    if order_total >= 40:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 0.80, 2)


def zone_13_standard(weight_kg: float, order_total: float) -> float:
    """Zone 13 (Beja area), standard delivery."""
    if order_total >= 75:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 1.20, 2)


def zone_13_express(weight_kg: float, order_total: float) -> float:
    """Zone 13 (Beja area), express delivery."""
    if order_total >= 150:
        return 0.0
    base = 11.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.50, 2)


def zone_14_economy(weight_kg: float, order_total: float) -> float:
    """Zone 14 (Faro area), economy delivery."""
    if order_total >= 40:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.50, 2)
    return round(base + 8 * 0.50 + (weight_kg - 10) * 0.80, 2)


def zone_14_standard(weight_kg: float, order_total: float) -> float:
    """Zone 14 (Faro area), standard delivery."""
    if order_total >= 60:
        return 0.0
    base = 4.60
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.10, 2)


def zone_14_express(weight_kg: float, order_total: float) -> float:
    """Zone 14 (Faro area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 11.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.90, 2)


def zone_15_economy(weight_kg: float, order_total: float) -> float:
    """Zone 15 (Portimao area), economy delivery."""
    if order_total >= 40:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 0.90, 2)


def zone_15_standard(weight_kg: float, order_total: float) -> float:
    """Zone 15 (Portimao area), standard delivery."""
    if order_total >= 80:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.35, 2)


def zone_15_express(weight_kg: float, order_total: float) -> float:
    """Zone 15 (Portimao area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 8.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.10, 2)
    return round(base + 8 * 1.10 + (weight_kg - 10) * 4.20, 2)


def zone_16_economy(weight_kg: float, order_total: float) -> float:
    """Zone 16 (Lagos area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 1.00, 2)


def zone_16_standard(weight_kg: float, order_total: float) -> float:
    """Zone 16 (Lagos area), standard delivery."""
    if order_total >= 80:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.35, 2)


def zone_16_express(weight_kg: float, order_total: float) -> float:
    """Zone 16 (Lagos area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 11.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.50, 2)


def zone_17_economy(weight_kg: float, order_total: float) -> float:
    """Zone 17 (Tavira area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 0.90, 2)


def zone_17_standard(weight_kg: float, order_total: float) -> float:
    """Zone 17 (Tavira area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 4.60
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.35, 2)


def zone_17_express(weight_kg: float, order_total: float) -> float:
    """Zone 17 (Tavira area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 8.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.50, 2)


def zone_18_economy(weight_kg: float, order_total: float) -> float:
    """Zone 18 (Sines area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 0.90, 2)


def zone_18_standard(weight_kg: float, order_total: float) -> float:
    """Zone 18 (Sines area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.10, 2)


def zone_18_express(weight_kg: float, order_total: float) -> float:
    """Zone 18 (Sines area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 11.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.10, 2)
    return round(base + 8 * 1.10 + (weight_kg - 10) * 4.20, 2)


def zone_19_economy(weight_kg: float, order_total: float) -> float:
    """Zone 19 (Peniche area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 1.00, 2)


def zone_19_standard(weight_kg: float, order_total: float) -> float:
    """Zone 19 (Peniche area), standard delivery."""
    if order_total >= 80:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.10, 2)


def zone_19_express(weight_kg: float, order_total: float) -> float:
    """Zone 19 (Peniche area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 9.80
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.50, 2)


def zone_20_economy(weight_kg: float, order_total: float) -> float:
    """Zone 20 (Cascais area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 1.00, 2)


def zone_20_standard(weight_kg: float, order_total: float) -> float:
    """Zone 20 (Cascais area), standard delivery."""
    if order_total >= 80:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.80, 2)
    return round(base + 8 * 0.80 + (weight_kg - 10) * 1.35, 2)


def zone_20_express(weight_kg: float, order_total: float) -> float:
    """Zone 20 (Cascais area), express delivery."""
    if order_total >= 100:
        return 0.0
    base = 9.80
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.10, 2)
    return round(base + 8 * 1.10 + (weight_kg - 10) * 4.20, 2)


def zone_21_economy(weight_kg: float, order_total: float) -> float:
    """Zone 21 (Sintra area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 0.80, 2)


def zone_21_standard(weight_kg: float, order_total: float) -> float:
    """Zone 21 (Sintra area), standard delivery."""
    if order_total >= 60:
        return 0.0
    base = 4.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.35, 2)


def zone_21_express(weight_kg: float, order_total: float) -> float:
    """Zone 21 (Sintra area), express delivery."""
    if order_total >= 100:
        return 0.0
    base = 8.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.20, 2)


def zone_22_economy(weight_kg: float, order_total: float) -> float:
    """Zone 22 (Almada area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 0.90, 2)


def zone_22_standard(weight_kg: float, order_total: float) -> float:
    """Zone 22 (Almada area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 3.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.20, 2)


def zone_22_express(weight_kg: float, order_total: float) -> float:
    """Zone 22 (Almada area), express delivery."""
    if order_total >= 150:
        return 0.0
    base = 10.50
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.50, 2)


def zone_23_economy(weight_kg: float, order_total: float) -> float:
    """Zone 23 (Amadora area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.50, 2)
    return round(base + 8 * 0.50 + (weight_kg - 10) * 1.00, 2)


def zone_23_standard(weight_kg: float, order_total: float) -> float:
    """Zone 23 (Amadora area), standard delivery."""
    if order_total >= 75:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.35, 2)


def zone_23_express(weight_kg: float, order_total: float) -> float:
    """Zone 23 (Amadora area), express delivery."""
    if order_total >= 150:
        return 0.0
    base = 9.80
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.90, 2)


def zone_24_economy(weight_kg: float, order_total: float) -> float:
    """Zone 24 (Loures area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.50, 2)
    return round(base + 8 * 0.50 + (weight_kg - 10) * 0.90, 2)


def zone_24_standard(weight_kg: float, order_total: float) -> float:
    """Zone 24 (Loures area), standard delivery."""
    if order_total >= 75:
        return 0.0
    base = 3.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.20, 2)


def zone_24_express(weight_kg: float, order_total: float) -> float:
    """Zone 24 (Loures area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 11.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.20, 2)


def zone_25_economy(weight_kg: float, order_total: float) -> float:
    """Zone 25 (Barreiro area), economy delivery."""
    if order_total >= 40:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 0.90, 2)


def zone_25_standard(weight_kg: float, order_total: float) -> float:
    """Zone 25 (Barreiro area), standard delivery."""
    if order_total >= 80:
        return 0.0
    base = 3.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.35, 2)


def zone_25_express(weight_kg: float, order_total: float) -> float:
    """Zone 25 (Barreiro area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 10.50
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.10, 2)
    return round(base + 8 * 1.10 + (weight_kg - 10) * 4.50, 2)


def zone_26_economy(weight_kg: float, order_total: float) -> float:
    """Zone 26 (Montijo area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.50, 2)
    return round(base + 8 * 0.50 + (weight_kg - 10) * 0.90, 2)


def zone_26_standard(weight_kg: float, order_total: float) -> float:
    """Zone 26 (Montijo area), standard delivery."""
    if order_total >= 80:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.20, 2)


def zone_26_express(weight_kg: float, order_total: float) -> float:
    """Zone 26 (Montijo area), express delivery."""
    if order_total >= 150:
        return 0.0
    base = 9.80
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.90, 2)


def zone_27_economy(weight_kg: float, order_total: float) -> float:
    """Zone 27 (Palmela area), economy delivery."""
    if order_total >= 40:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 0.90, 2)


def zone_27_standard(weight_kg: float, order_total: float) -> float:
    """Zone 27 (Palmela area), standard delivery."""
    if order_total >= 80:
        return 0.0
    base = 4.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 1.10, 2)


def zone_27_express(weight_kg: float, order_total: float) -> float:
    """Zone 27 (Palmela area), express delivery."""
    if order_total >= 150:
        return 0.0
    base = 11.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.50, 2)


def zone_28_economy(weight_kg: float, order_total: float) -> float:
    """Zone 28 (Seixal area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 1.00, 2)


def zone_28_standard(weight_kg: float, order_total: float) -> float:
    """Zone 28 (Seixal area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.10, 2)


def zone_28_express(weight_kg: float, order_total: float) -> float:
    """Zone 28 (Seixal area), express delivery."""
    if order_total >= 100:
        return 0.0
    base = 11.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.50, 2)


def zone_29_economy(weight_kg: float, order_total: float) -> float:
    """Zone 29 (Mafra area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 1.00, 2)


def zone_29_standard(weight_kg: float, order_total: float) -> float:
    """Zone 29 (Mafra area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.20, 2)


def zone_29_express(weight_kg: float, order_total: float) -> float:
    """Zone 29 (Mafra area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 10.50
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.50, 2)


def zone_30_economy(weight_kg: float, order_total: float) -> float:
    """Zone 30 (Torres Vedras area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 1.00, 2)


def zone_30_standard(weight_kg: float, order_total: float) -> float:
    """Zone 30 (Torres Vedras area), standard delivery."""
    if order_total >= 75:
        return 0.0
    base = 4.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 1.20, 2)


def zone_30_express(weight_kg: float, order_total: float) -> float:
    """Zone 30 (Torres Vedras area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 8.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.50, 2)


def zone_31_economy(weight_kg: float, order_total: float) -> float:
    """Zone 31 (Lisbon area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 0.90, 2)


def zone_31_standard(weight_kg: float, order_total: float) -> float:
    """Zone 31 (Lisbon area), standard delivery."""
    if order_total >= 120:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 1.50, 2)


def zone_31_express(weight_kg: float, order_total: float) -> float:
    """Zone 31 (Lisbon area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 9.80
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.50, 2)


def zone_32_economy(weight_kg: float, order_total: float) -> float:
    """Zone 32 (Caldas area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.50, 2)
    return round(base + 8 * 0.50 + (weight_kg - 10) * 1.00, 2)


def zone_32_standard(weight_kg: float, order_total: float) -> float:
    """Zone 32 (Caldas area), standard delivery."""
    if order_total >= 80:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.10, 2)


def zone_32_express(weight_kg: float, order_total: float) -> float:
    """Zone 32 (Caldas area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 9.80
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.10, 2)
    return round(base + 8 * 1.10 + (weight_kg - 10) * 4.50, 2)


def zone_33_economy(weight_kg: float, order_total: float) -> float:
    """Zone 33 (Nazare area), economy delivery."""
    if order_total >= 40:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 0.80, 2)


def zone_33_standard(weight_kg: float, order_total: float) -> float:
    """Zone 33 (Nazare area), standard delivery."""
    if order_total >= 75:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.10, 2)


def zone_33_express(weight_kg: float, order_total: float) -> float:
    """Zone 33 (Nazare area), express delivery."""
    if order_total >= 150:
        return 0.0
    base = 8.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.20, 2)


def zone_34_economy(weight_kg: float, order_total: float) -> float:
    """Zone 34 (Tomar area), economy delivery."""
    if order_total >= 40:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 0.90, 2)


def zone_34_standard(weight_kg: float, order_total: float) -> float:
    """Zone 34 (Tomar area), standard delivery."""
    if order_total >= 80:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.10, 2)


def zone_34_express(weight_kg: float, order_total: float) -> float:
    """Zone 34 (Tomar area), express delivery."""
    if order_total >= 150:
        return 0.0
    base = 10.50
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.20, 2)
    return round(base + 8 * 1.20 + (weight_kg - 10) * 4.20, 2)


def zone_35_economy(weight_kg: float, order_total: float) -> float:
    """Zone 35 (Abrantes area), economy delivery."""
    if order_total >= 40:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.50, 2)
    return round(base + 8 * 0.50 + (weight_kg - 10) * 0.80, 2)


def zone_35_standard(weight_kg: float, order_total: float) -> float:
    """Zone 35 (Abrantes area), standard delivery."""
    if order_total >= 60:
        return 0.0
    base = 3.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.10, 2)


def zone_35_express(weight_kg: float, order_total: float) -> float:
    """Zone 35 (Abrantes area), express delivery."""
    if order_total >= 150:
        return 0.0
    base = 9.80
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.10, 2)
    return round(base + 8 * 1.10 + (weight_kg - 10) * 4.50, 2)


def zone_36_economy(weight_kg: float, order_total: float) -> float:
    """Zone 36 (Portalegre area), economy delivery."""
    if order_total >= 40:
        return 0.0
    base = 3.10
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 1.00, 2)


def zone_36_standard(weight_kg: float, order_total: float) -> float:
    """Zone 36 (Portalegre area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.10, 2)


def zone_36_express(weight_kg: float, order_total: float) -> float:
    """Zone 36 (Portalegre area), express delivery."""
    if order_total >= 150:
        return 0.0
    base = 10.50
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.10, 2)
    return round(base + 8 * 1.10 + (weight_kg - 10) * 4.50, 2)


def zone_37_economy(weight_kg: float, order_total: float) -> float:
    """Zone 37 (Elvas area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.50, 2)
    return round(base + 8 * 0.50 + (weight_kg - 10) * 0.90, 2)


def zone_37_standard(weight_kg: float, order_total: float) -> float:
    """Zone 37 (Elvas area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 3.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.70, 2)
    return round(base + 8 * 0.70 + (weight_kg - 10) * 1.35, 2)


def zone_37_express(weight_kg: float, order_total: float) -> float:
    """Zone 37 (Elvas area), express delivery."""
    if order_total >= 150:
        return 0.0
    base = 11.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.10, 2)
    return round(base + 8 * 1.10 + (weight_kg - 10) * 4.50, 2)


def zone_38_economy(weight_kg: float, order_total: float) -> float:
    """Zone 38 (Braganca area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 3.40
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 0.90, 2)


def zone_38_standard(weight_kg: float, order_total: float) -> float:
    """Zone 38 (Braganca area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 4.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.60, 2)
    return round(base + 8 * 0.60 + (weight_kg - 10) * 1.20, 2)


def zone_38_express(weight_kg: float, order_total: float) -> float:
    """Zone 38 (Braganca area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 8.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.50, 2)


def zone_39_economy(weight_kg: float, order_total: float) -> float:
    """Zone 39 (Chaves area), economy delivery."""
    if order_total >= 60:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.40, 2)
    return round(base + 8 * 0.40 + (weight_kg - 10) * 0.90, 2)


def zone_39_standard(weight_kg: float, order_total: float) -> float:
    """Zone 39 (Chaves area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 3.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.80, 2)
    return round(base + 8 * 0.80 + (weight_kg - 10) * 1.35, 2)


def zone_39_express(weight_kg: float, order_total: float) -> float:
    """Zone 39 (Chaves area), express delivery."""
    if order_total >= 120:
        return 0.0
    base = 11.20
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.50, 2)


def zone_40_economy(weight_kg: float, order_total: float) -> float:
    """Zone 40 (Vila Real area), economy delivery."""
    if order_total >= 50:
        return 0.0
    base = 2.90
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.50, 2)
    return round(base + 8 * 0.50 + (weight_kg - 10) * 0.90, 2)


def zone_40_standard(weight_kg: float, order_total: float) -> float:
    """Zone 40 (Vila Real area), standard delivery."""
    if order_total >= 50:
        return 0.0
    base = 4.60
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 0.90, 2)
    return round(base + 8 * 0.90 + (weight_kg - 10) * 1.50, 2)


def zone_40_express(weight_kg: float, order_total: float) -> float:
    """Zone 40 (Vila Real area), express delivery."""
    if order_total >= 100:
        return 0.0
    base = 9.80
    if weight_kg <= 2:
        return round(base, 2)
    if weight_kg <= 10:
        return round(base + (weight_kg - 2) * 1.40, 2)
    return round(base + 8 * 1.40 + (weight_kg - 10) * 4.20, 2)


RULES = {
    (1, "economy"): zone_01_economy,
    (1, "standard"): zone_01_standard,
    (1, "express"): zone_01_express,
    (2, "economy"): zone_02_economy,
    (2, "standard"): zone_02_standard,
    (2, "express"): zone_02_express,
    (3, "economy"): zone_03_economy,
    (3, "standard"): zone_03_standard,
    (3, "express"): zone_03_express,
    (4, "economy"): zone_04_economy,
    (4, "standard"): zone_04_standard,
    (4, "express"): zone_04_express,
    (5, "economy"): zone_05_economy,
    (5, "standard"): zone_05_standard,
    (5, "express"): zone_05_express,
    (6, "economy"): zone_06_economy,
    (6, "standard"): zone_06_standard,
    (6, "express"): zone_06_express,
    (7, "economy"): zone_07_economy,
    (7, "standard"): zone_07_standard,
    (7, "express"): zone_07_express,
    (8, "economy"): zone_08_economy,
    (8, "standard"): zone_08_standard,
    (8, "express"): zone_08_express,
    (9, "economy"): zone_09_economy,
    (9, "standard"): zone_09_standard,
    (9, "express"): zone_09_express,
    (10, "economy"): zone_10_economy,
    (10, "standard"): zone_10_standard,
    (10, "express"): zone_10_express,
    (11, "economy"): zone_11_economy,
    (11, "standard"): zone_11_standard,
    (11, "express"): zone_11_express,
    (12, "economy"): zone_12_economy,
    (12, "standard"): zone_12_standard,
    (12, "express"): zone_12_express,
    (13, "economy"): zone_13_economy,
    (13, "standard"): zone_13_standard,
    (13, "express"): zone_13_express,
    (14, "economy"): zone_14_economy,
    (14, "standard"): zone_14_standard,
    (14, "express"): zone_14_express,
    (15, "economy"): zone_15_economy,
    (15, "standard"): zone_15_standard,
    (15, "express"): zone_15_express,
    (16, "economy"): zone_16_economy,
    (16, "standard"): zone_16_standard,
    (16, "express"): zone_16_express,
    (17, "economy"): zone_17_economy,
    (17, "standard"): zone_17_standard,
    (17, "express"): zone_17_express,
    (18, "economy"): zone_18_economy,
    (18, "standard"): zone_18_standard,
    (18, "express"): zone_18_express,
    (19, "economy"): zone_19_economy,
    (19, "standard"): zone_19_standard,
    (19, "express"): zone_19_express,
    (20, "economy"): zone_20_economy,
    (20, "standard"): zone_20_standard,
    (20, "express"): zone_20_express,
    (21, "economy"): zone_21_economy,
    (21, "standard"): zone_21_standard,
    (21, "express"): zone_21_express,
    (22, "economy"): zone_22_economy,
    (22, "standard"): zone_22_standard,
    (22, "express"): zone_22_express,
    (23, "economy"): zone_23_economy,
    (23, "standard"): zone_23_standard,
    (23, "express"): zone_23_express,
    (24, "economy"): zone_24_economy,
    (24, "standard"): zone_24_standard,
    (24, "express"): zone_24_express,
    (25, "economy"): zone_25_economy,
    (25, "standard"): zone_25_standard,
    (25, "express"): zone_25_express,
    (26, "economy"): zone_26_economy,
    (26, "standard"): zone_26_standard,
    (26, "express"): zone_26_express,
    (27, "economy"): zone_27_economy,
    (27, "standard"): zone_27_standard,
    (27, "express"): zone_27_express,
    (28, "economy"): zone_28_economy,
    (28, "standard"): zone_28_standard,
    (28, "express"): zone_28_express,
    (29, "economy"): zone_29_economy,
    (29, "standard"): zone_29_standard,
    (29, "express"): zone_29_express,
    (30, "economy"): zone_30_economy,
    (30, "standard"): zone_30_standard,
    (30, "express"): zone_30_express,
    (31, "economy"): zone_31_economy,
    (31, "standard"): zone_31_standard,
    (31, "express"): zone_31_express,
    (32, "economy"): zone_32_economy,
    (32, "standard"): zone_32_standard,
    (32, "express"): zone_32_express,
    (33, "economy"): zone_33_economy,
    (33, "standard"): zone_33_standard,
    (33, "express"): zone_33_express,
    (34, "economy"): zone_34_economy,
    (34, "standard"): zone_34_standard,
    (34, "express"): zone_34_express,
    (35, "economy"): zone_35_economy,
    (35, "standard"): zone_35_standard,
    (35, "express"): zone_35_express,
    (36, "economy"): zone_36_economy,
    (36, "standard"): zone_36_standard,
    (36, "express"): zone_36_express,
    (37, "economy"): zone_37_economy,
    (37, "standard"): zone_37_standard,
    (37, "express"): zone_37_express,
    (38, "economy"): zone_38_economy,
    (38, "standard"): zone_38_standard,
    (38, "express"): zone_38_express,
    (39, "economy"): zone_39_economy,
    (39, "standard"): zone_39_standard,
    (39, "express"): zone_39_express,
    (40, "economy"): zone_40_economy,
    (40, "standard"): zone_40_standard,
    (40, "express"): zone_40_express,
}


def price(zone: int, service: str, weight_kg: float, order_total: float) -> float:
    """The shipping price for a parcel; KeyError for an unknown zone or service."""
    if weight_kg <= 0:
        raise ValueError("weight must be positive")
    return RULES[(zone, service)](weight_kg, order_total)
