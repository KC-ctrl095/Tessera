"""Times as the clinic writes them: 'YYYY-MM-DDTHH:MM' strings, local time."""

from datetime import date, datetime, timedelta

FORMAT = "%Y-%m-%dT%H:%M"


def parse(text) -> datetime:
    return datetime.strptime(text, FORMAT)


def fmt(value: datetime) -> str:
    return value.strftime(FORMAT)


def times_between(start="09:00", end="17:00", minutes=30) -> list:
    """['09:00', '09:30', ...] up to (not including) `end`."""
    t = datetime.strptime(start, "%H:%M")
    stop = datetime.strptime(end, "%H:%M")
    out = []
    while t < stop:
        out.append(t.strftime("%H:%M"))
        t += timedelta(minutes=minutes)
    return out


def day_before_at(starts_at: str, hhmm: str) -> str:
    """The day before an appointment, at hhmm."""
    day = parse(starts_at).date() - timedelta(days=1)
    return f"{day.isoformat()}T{hhmm}"


def age_on(birth_date: str, today: date) -> int:
    born = date.fromisoformat(birth_date)
    return today.year - born.year - ((today.month, today.day) < (born.month, born.day))
