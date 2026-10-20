"""Text normalisation shared by the data team (vendored copy, v0.9.2; don't edit here)."""

import re
import unicodedata

_SPACES = re.compile(r"\s+")
_DOTLESS_DOMAINS = {"gmail.com", "googlemail.com"}


def normalize_name(value: str) -> str:
    """Compatibility-normalised, case-folded, with runs of whitespace collapsed."""
    value = unicodedata.normalize("NFKC", value)
    return _SPACES.sub(" ", value).strip().casefold()


def normalize_email(value: str) -> str:
    """Lower-cased; "+tags" dropped; dots dropped for providers that ignore them."""
    value = value.strip().lower()
    if value.count("@") != 1:
        return value
    local, domain = value.split("@")
    local = local.split("+", 1)[0]
    if domain in _DOTLESS_DOMAINS:
        local = local.replace(".", "")
        domain = "gmail.com"
    if not local:
        return ""
    return f"{local}@{domain}"
