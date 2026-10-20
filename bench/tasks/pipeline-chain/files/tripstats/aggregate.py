"""Stage 5: totals per district and per station."""

from collections import Counter, defaultdict


def median(values):
    values = sorted(values)
    n = len(values)
    if n == 0:
        return 0.0
    mid = n // 2
    return values[mid]


def aggregate(trips):
    districts = defaultdict(lambda: {"started": 0, "ended": 0, "casual": 0, "km": 0.0, "minutes": [], "hours": Counter()})
    stations = defaultdict(lambda: {"district": "", "started": 0, "ended": 0})
    for t in trips:
        d = districts[t["start_district"]]
        d["started"] += 1
        d["casual"] += t["casual"]
        d["km"] += t["km"]
        d["minutes"].append(t["seconds"] / 60)
        d["hours"][t["start"].hour] += 1
        districts[t["end_district"]]["ended"] += 1
        s = stations[t["start_station"]]
        s["district"] = t["start_district"]
        s["started"] += 1
        e = stations[t["end_station"]]
        e["district"] = t["end_district"]
        e["ended"] += 1
    result = {}
    for name, d in districts.items():
        busiest = min(d["hours"], key=lambda h: (-d["hours"][h], h)) if d["hours"] else None
        result[name] = {
            "started": d["started"],
            "ended": d["ended"],
            "casual_pct": 100 * d["casual"] / d["started"] if d["started"] else 0.0,
            "km": d["km"],
            "median_min": median(d["minutes"]),
            "busiest_hour": busiest,
        }
    return result, dict(stations)
