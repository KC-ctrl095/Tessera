"""Plain-text tables for the reports emailed to finance."""

from core.money import format_eur


def money_table(rows, total=None) -> str:
    """Two columns, amounts right-aligned; a total line when given."""
    cells = [(str(label), format_eur(cents)) for label, cents in rows]
    if total is not None:
        cells.append(("total", format_eur(total)))
    width = max((len(label) for label, _ in cells), default=5)
    amount_width = max((len(amount) for _, amount in cells), default=5)
    lines = [f"{label:<{width}}  {amount:>{amount_width}}" for label, amount in cells]
    if total is not None:
        lines.insert(-1, "-" * (width + 2 + amount_width))
    return "\n".join(lines)
