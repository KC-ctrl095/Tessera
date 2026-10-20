"""VAT by the customer's country (standard rates, percent)."""

from core.errors import InvalidRequest

VAT_RATES = {
    "PT": 23,
    "ES": 21,
    "FR": 20,
    "DE": 19,
    "NL": 21,
    "IE": 23,
    "BE": 21,
}


def vat_rate(country: str) -> int:
    try:
        return VAT_RATES[country.upper()]
    except KeyError:
        raise InvalidRequest(f"we don't ship to {country!r}") from None
