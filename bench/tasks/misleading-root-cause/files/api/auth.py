"""API keys of partner integrations (the demo data's keys are made up)."""

KEYS = {
    "demo-key-casa-do-grao": 3,
    "demo-key-office-hub": 11,
    "demo-key-roastery-lab": 27,
}


def customer_for(headers: dict):
    """The customer id an Authorization header belongs to, or None."""
    value = headers.get("Authorization", "")
    scheme, _, key = value.partition(" ")
    if scheme != "Key" or not key:
        return None
    return KEYS.get(key)
