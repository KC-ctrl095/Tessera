"""Stage 6: the report text."""


def render(month, counted, dropped, replaced, districts, stations):
    lines = [
        f"Bike trips, {month}",
        f"Trips counted: {counted} (dropped: {dropped['too_short']} too short, {dropped['too_long']} too long; "
        f"duplicates replaced: {replaced})",
        "",
        f"{'District':<16}{'Started':>8}{'Ended':>8}{'Casual%':>9}{'Km':>10}{'Median min':>12}{'Busiest':>9}",
    ]
    # Most trips first; ties in alphabetical order.
    for name, d in sorted(districts.items(), key=lambda kv: (-kv[1]["started"], kv[0])):
        hour = "-" if d["busiest_hour"] is None else f"{d['busiest_hour']:02d}:00"
        lines.append(
            f"{name:<16}{d['started']:>8}{d['ended']:>8}{d['casual_pct']:>9.1f}{d['km']:>10.1f}{d['median_min']:>12.1f}{hour:>9}"
        )
    lines += ["", f"{'Station':<34}{'District':<16}{'Started':>8}{'Ended':>8}"]
    for name, s in sorted(stations.items(), key=lambda kv: (-kv[1]["started"], -kv[1]["ended"], kv[0])):
        lines.append(f"{name:<34}{s['district']:<16}{s['started']:>8}{s['ended']:>8}")
    return "\n".join(lines) + "\n"
