"""Turns due reminders into messages for the SMS and email gateways."""

from . import reminders, settings_store


def build(now_iso):
    """[(channel, to, text)] for every due reminder; marks them sent."""
    clinic = settings_store.get("clinic_name")
    out = []
    ids = []
    for r in reminders.due(now_iso):
        if not r["to"]:
            continue
        when = r["starts_at"].replace("T", " at ")
        out.append((r["channel"], r["to"], f"{clinic}: reminder for {r['name']}, appointment on {when}."))
        ids.append(r["id"])
    reminders.mark_sent(ids)
    return out
