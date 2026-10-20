from dataclasses import dataclass
from typing import Optional


@dataclass(frozen=True)
class Customer:
    id: int
    name: str
    country: str
    kind: str = "business"  # "business" or "consumer"
    vat_id: Optional[str] = None
    email: str = ""

    @property
    def is_business(self) -> bool:
        return self.kind == "business"
