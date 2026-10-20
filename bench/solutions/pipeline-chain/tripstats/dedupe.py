"""Stage 3: the export repeats a trip when it was corrected; the last row wins."""


def dedupe(trips):
    """(trips, replaced): one trip per trip_id, the last version, in order of first appearance."""
    latest, order = {}, []
    for t in trips:
        if t["trip_id"] not in latest:
            order.append(t["trip_id"])
        latest[t["trip_id"]] = t
    return [latest[i] for i in order], len(trips) - len(order)
