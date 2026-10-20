"""Which VAT rate applies to an invoice line."""

from decimal import Decimal

from ..countries import EU
from .rates import rate

ZERO = Decimal("0")


def vat_rate_for(seller_country: str, customer, product) -> Decimal:
    """The VAT percentage to charge for this product sold to this customer.

    - Sales to customers outside the EU are exports: no VAT.
    - Everything else is charged at the seller's rate for the product's
      category (consumers in other EU countries too: we stay under the OSS
      threshold).
    """
    if customer.country not in EU:
        return ZERO
    return rate(seller_country, product.vat_category)
