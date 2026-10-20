"""Payment reminders for overdue invoices."""


def reminder_text(invoice, today):
    days = (today - invoice.due_on).days
    if days <= 0:
        return None
    return f"Invoice {invoice.number} is {days} day{'s' if days != 1 else ''} overdue ({invoice.gross_total:.2f} {invoice.currency})."
