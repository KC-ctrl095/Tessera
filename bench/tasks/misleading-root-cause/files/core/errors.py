class NotFound(Exception):
    """A customer, subscription, payment or invoice that doesn't exist."""


class InvalidRequest(Exception):
    """Input that can't be used (a bad month, a negative amount...)."""
