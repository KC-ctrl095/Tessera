class NotFound(Exception):
    """No such patient, slot, appointment, invoice..."""


class Conflict(Exception):
    """The request clashes with the current state (slot taken, already cancelled...)."""


class Invalid(Exception):
    """The input can't be used."""
