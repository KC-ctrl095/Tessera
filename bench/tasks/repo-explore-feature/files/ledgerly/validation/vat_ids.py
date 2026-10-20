"""Checks of VAT identification numbers (format only, plus the Portuguese
check digit). Added in 3.5 for the customer import; see docs/CHANGELOG.md."""

import re

# Country prefix + national format, from the EU's VIES documentation.
FORMATS = {
    "AT": r"ATU\d{8}", "BE": r"BE[01]\d{9}", "BG": r"BG\d{9,10}", "CY": r"CY\d{8}[A-Z]", "CZ": r"CZ\d{8,10}",
    "DE": r"DE\d{9}", "DK": r"DK\d{8}", "EE": r"EE\d{9}", "ES": r"ES[A-Z0-9]\d{7}[A-Z0-9]", "FI": r"FI\d{8}",
    "FR": r"FR[A-HJ-NP-Z0-9]{2}\d{9}", "GR": r"EL\d{9}", "HR": r"HR\d{11}", "HU": r"HU\d{8}", "IE": r"IE\d{7}[A-W][A-I]?",
    "IT": r"IT\d{11}", "LT": r"LT(\d{9}|\d{12})", "LU": r"LU\d{8}", "LV": r"LV\d{11}", "MT": r"MT\d{8}",
    "NL": r"NL\d{9}B\d{2}", "PL": r"PL\d{10}", "PT": r"PT\d{9}", "RO": r"RO\d{2,10}", "SE": r"SE\d{12}",
    "SI": r"SI\d{8}", "SK": r"SK\d{10}",
}


def normalize(vat_id: str) -> str:
    return re.sub(r"[\s.\-]", "", vat_id or "").upper()


def _pt_check_digit_ok(digits: str) -> bool:
    total = sum(int(d) * w for d, w in zip(digits[:8], range(9, 1, -1)))
    check = 11 - total % 11
    return (0 if check >= 10 else check) == int(digits[8])


def is_valid_vat_id(country: str, vat_id: str) -> bool:
    """True when vat_id is a well-formed VAT number of that EU country."""
    pattern = FORMATS.get(country)
    if not pattern or not vat_id:
        return False
    v = normalize(vat_id)
    if not re.fullmatch(pattern, v):
        return False
    if country == "PT":
        return _pt_check_digit_ok(v[2:])
    return True
