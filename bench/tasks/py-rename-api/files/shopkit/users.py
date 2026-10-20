"""Users of the shop (customers and staff)."""

from dataclasses import dataclass

from .store import STORE


@dataclass(frozen=True)
class UsrRecord:
    id: int
    name: str
    email: str
    staff: bool = False

    def display(self) -> str:
        role = " (staff)" if self.staff else ""
        return f"{self.name} <{self.email}>{role}"


def get_usr(user_id: int) -> UsrRecord:
    """The user with this id; KeyError when there is none."""
    row = STORE["users"][user_id]
    return UsrRecord(id=user_id, **row)


def list_users(staff=None):
    """Every user, by id; `staff=True/False` keeps only staff or customers."""
    users = [get_usr(uid) for uid in sorted(STORE["users"])]
    if staff is not None:
        users = [u for u in users if u.staff == staff]
    return users
