from dataclasses import dataclass
from decimal import Decimal


@dataclass(frozen=True)
class Product:
    sku: str
    name: str
    unit_price: Decimal
    vat_category: str = "standard"  # standard | intermediate | reduced | exempt
