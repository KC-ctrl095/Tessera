"""Short random ids for public links."""

import secrets

ALPHABET = "23456789abcdefghjkmnpqrstuvwxyz"


def short_id(n=10):
    return "".join(secrets.choice(ALPHABET) for _ in range(n))
