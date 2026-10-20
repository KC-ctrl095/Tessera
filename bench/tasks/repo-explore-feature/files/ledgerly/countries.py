"""Country lists used across ledgerly (ISO 3166-1 alpha-2 codes)."""

# Member states of the European Union.
EU = frozenset({
    "AT", "BE", "BG", "CY", "CZ", "DE", "DK", "EE", "ES", "FI", "FR", "GR", "HR", "HU",
    "IE", "IT", "LT", "LU", "LV", "MT", "NL", "PL", "PT", "RO", "SE", "SI", "SK",
})
# The European Economic Area: the EU plus Iceland, Liechtenstein and Norway.
# (Single market for goods; NOT part of the EU VAT area.)
EEA = EU | {"IS", "LI", "NO"}

NAMES = {
    "AT": "Austria", "BE": "Belgium", "BG": "Bulgaria", "CY": "Cyprus", "CZ": "Czechia", "DE": "Germany",
    "DK": "Denmark", "EE": "Estonia", "ES": "Spain", "FI": "Finland", "FR": "France", "GR": "Greece",
    "HR": "Croatia", "HU": "Hungary", "IE": "Ireland", "IT": "Italy", "LT": "Lithuania", "LU": "Luxembourg",
    "LV": "Latvia", "MT": "Malta", "NL": "Netherlands", "PL": "Poland", "PT": "Portugal", "RO": "Romania",
    "SE": "Sweden", "SI": "Slovenia", "SK": "Slovakia", "IS": "Iceland", "LI": "Liechtenstein", "NO": "Norway",
    "CH": "Switzerland", "GB": "United Kingdom", "US": "United States", "BR": "Brazil", "CA": "Canada",
}


def name(code: str) -> str:
    return NAMES.get(code, code)
