"""Orders and who placed them."""

from dataclasses import dataclass

from .store import STORE
from .users import UsrRecord, get_usr


@dataclass(frozen=True)
class Order:
    id: int
    customer: UsrRecord
    total_cents: int


def orders_for(user_id: int):
    """The orders a user placed, oldest first."""
    customer = get_usr(user_id)
    return [
        Order(id=o["id"], customer=customer, total_cents=o["total_cents"])
        for o in STORE["orders"]
        if o["user_id"] == user_id
    ]
