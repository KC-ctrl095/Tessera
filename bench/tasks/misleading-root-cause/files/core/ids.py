"""Display ids: SUB-0042, PAY-001234, CUS-0007."""


def sub_id(n: int) -> str:
    return f"SUB-{n:04d}"


def payment_id(n: int) -> str:
    return f"PAY-{n:06d}"


def customer_id(n: int) -> str:
    return f"CUS-{n:04d}"


def parse_id(text: str) -> int:
    """'SUB-0042' -> 42 (any prefix)."""
    _, _, number = text.partition("-")
    if not number.isdigit():
        raise ValueError(f"not an id: {text!r}")
    return int(number)
