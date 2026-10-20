"""Turns build events into short chat messages."""

MAX_LEN = 2000


def format_event(event):
    """A one-line summary of a build event (a dict from the build server)."""
    status = event.get("status", "unknown")
    mark = {"passed": "OK", "failed": "FAILED"}.get(status, status.upper())
    text = f"[{mark}] {event.get('project', '?')} #{event.get('number', '?')}"
    if event.get("branch"):
        text += f" on {event['branch']}"
    if event.get("summary"):
        text += f": {event['summary']}"
    if len(text) > MAX_LEN:
        text = text[: MAX_LEN - 1] + "…"
    return text


def wants(room, event):
    """Whether a room (a dict from rooms.toml) should get this event."""
    if room.get("only_failures"):
        return event.get("status") == "failed"
    return True
