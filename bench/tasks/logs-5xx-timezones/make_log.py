"""Writes files/access.log for the logs-5xx-timezones task: python3 make_log.py

Two web servers' access logs (nginx "combined" format) merged by time: web-a
logs in +0200, web-b in +0000. Fixed seed, so the file is the same every time.
On top of ordinary traffic it plants errors that catch the usual mistakes
(local time taken as UTC, the offset applied the wrong way, query strings kept,
4xx counted) and refuses to write a log where a mistake gives the right answer.
"""

import random
import re
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

SEED = 20260912
DAY = datetime(2026, 9, 12, tzinfo=timezone.utc)
WINDOW = (DAY.replace(hour=14), DAY.replace(hour=15))
SERVERS = {"a": timezone(timedelta(hours=2)), "b": timezone.utc}
MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
AGENTS = [
    "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0 Safari/537.36",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_6) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Safari/605.1.15",
    "okhttp/4.12.0",
    "kuma-monitor/1.23",
]
SEARCH_WORDS = ["kettle", "grinder", "filters", "mug", "scale", "beans", "decaf", "tamper", "jar", "gift", "sale", "oat milk"]


def fmt_time(t, server):
    local = t.astimezone(SERVERS[server])
    off = local.strftime("%z")
    return f"{local.day:02d}/{MONTHS[local.month - 1]}/{local.year}:{local:%H:%M:%S} {off}"


def query_for(rng, path):
    if path == "/api/search":
        q = rng.choice(SEARCH_WORDS).replace(" ", "+")
        extra = rng.choice(["", f"&page={rng.randint(2, 6)}", "&sort=price", f"&page={rng.randint(2, 6)}&sort=new"])
        return f"?q={q}{extra}"
    if path == "/api/products" and rng.random() < 0.6:
        return f"?category={rng.choice(['brewing', 'beans', 'cups', 'spares'])}&page={rng.randint(1, 4)}"
    if path == "/api/orders" and rng.random() < 0.4:
        return f"?status={rng.choice(['open', 'shipped'])}"
    if path == "/api/stock" and rng.random() < 0.3:
        return f"?sku=SKU-{rng.randint(1000, 1099)}"
    return ""


def build():
    rng = random.Random(SEED)
    events = []  # (utc time, server, method, path+query, status)

    def at(lo, hi):
        return lo + timedelta(seconds=rng.randrange(int((hi - lo).total_seconds())))

    def add(t, path, status, method="GET", server=None, query=None):
        server = server or rng.choice("ab")
        q = query_for(rng, path) if query is None else query
        events.append((t, server, method, path + q, status))

    # Ordinary traffic, 10:00 to 19:00 UTC.
    weights = {
        "/": 10, "/static/app.js": 9, "/static/app.css": 9, "/api/products": 14, "/api/search": 10,
        "/api/cart": 9, "/api/checkout": 4, "/api/orders": 6, "/api/login": 4, "/api/stock": 4,
        "/api/recommendations": 5, "/api/invoices": 2, "/healthz": 6,
    }
    paths = list(weights)
    start, end = DAY.replace(hour=10), DAY.replace(hour=19)
    for _ in range(2900):
        path = rng.choices(paths, [weights[p] for p in paths])[0]
        method = "POST" if path in ("/api/cart", "/api/checkout", "/api/login") and rng.random() < 0.7 else "GET"
        r = rng.random()
        status = 200 if r < 0.86 else 304 if r < 0.91 else 404 if r < 0.97 else 401 if r < 0.995 else rng.choice([500, 502])
        add(at(start, end), path, status, method)

    w0, w1 = WINDOW
    # The incident: checkout and stock fail, search times out for many queries.
    for _ in range(17):
        add(at(w0, w1), "/api/checkout", rng.choice([500, 502, 502, 504]), rng.choice(["POST", "POST", "GET"]))
    for _ in range(14):
        add(at(w0, w1), "/api/search", rng.choice([502, 504]))
    for _ in range(10):
        add(at(w0, w1), "/api/stock", rng.choice([500, 503]))
    for _ in range(5):
        add(at(w0, w1), "/api/orders", 502)
    # Just outside the hour.
    for lo, hi in ((w0 - timedelta(minutes=3), w0 - timedelta(seconds=1)), (w1 + timedelta(seconds=1), w1 + timedelta(minutes=4))):
        for _ in range(4):
            add(at(lo, hi), "/api/orders", 503)
    # Logins rejected in the hour: 4xx, not 5xx.
    for _ in range(30):
        add(at(w0, w1), "/api/login", 401, "POST")
    # web-a errors whose local clock reads 14:00-15:00 (12:00-13:00 UTC) ...
    for _ in range(26):
        add(at(DAY.replace(hour=12), DAY.replace(hour=13)), "/api/recommendations", rng.choice([500, 503]), server="a")
    # ... and web-a errors that land in the hour when the offset is applied the
    # wrong way round (10:00-11:00 UTC, 12:00-13:00 on web-a's clock).
    for _ in range(24):
        add(at(DAY.replace(hour=10), DAY.replace(hour=11)), "/api/invoices", 500, server="a")

    events.sort(key=lambda e: (e[0], e[1]))
    lines = []
    for t, server, method, target, status in events:
        ip = rng.choice(["192.0.2.", "198.51.100.", "203.0.113."]) + str(rng.randint(2, 254))
        size = 0 if status == 304 else rng.randint(180, 1200) if status >= 400 else rng.randint(300, 48000)
        ref = "-" if target.startswith(("/api/", "/healthz")) or rng.random() < 0.5 else "https://brewbench.example/"
        agent = "kuma-monitor/1.23" if target == "/healthz" else rng.choice(AGENTS[:5])
        lines.append(f'{ip} - - [{fmt_time(t, server)}] "{method} {target} HTTP/1.1" {status} {size} "{ref}" "{agent}"')

    # A few lines that aren't requests.
    def insert_near(minute_utc, text):
        idx = next(i for i, (t, *_rest) in enumerate(events) if t >= DAY.replace(hour=minute_utc[0], minute=minute_utc[1]))
        lines.insert(idx, text)
        events.insert(idx, (events[idx][0], "x", "", "", 0))

    insert_near((11, 5), '203.0.113.77 - - [12/Sep/2026:13:05:12 +0200] "\\x16\\x03\\x01\\x00\\xa5\\x01\\x00\\x00\\xa1\\x03\\x03" 400 157 "-" "-"')
    insert_near((14, 20), '198.51.100.23 - - [12/Sep/2026:14:20:31 +0000] "-" 400 0 "-" "-"')
    insert_near((14, 41), '192.0.2.61 - - [12/Sep/2026:16:41:09 +0200] "GET /api/checkout HTTP/1.1')
    insert_near((17, 30), "")
    insert_near((18, 2), '203.0.113.9 - - [12/Sep/2026:18:02:44 +0000] "GET /wp-login.php HTTP/1.1" 404 153 "-" "Mozilla/5.0"')
    return "\n".join(lines) + "\n"


LINE = re.compile(
    r'^(\S+) \S+ \S+ \[(\d{2})/(\w{3})/(\d{4}):(\d{2}):(\d{2}):(\d{2}) ([+-])(\d{2})(\d{2})\] '
    r'"(\S+) (\S+) HTTP/[\d.]+" (\d{3}) (\d+|-) "[^"]*" "[^"]*"$'
)


def parse(text):
    """(local time, offset minutes, path with query, status) for every well-formed line."""
    out = []
    for line in text.splitlines():
        m = LINE.match(line)
        if not m:
            continue
        _ip, d, mon, y, hh, mm, ss, sign, oh, om, _method, target, status, _size = m.groups()
        local = datetime(int(y), MONTHS.index(mon) + 1, int(d), int(hh), int(mm), int(ss))
        offset = (1 if sign == "+" else -1) * (int(oh) * 60 + int(om))
        out.append((local, offset, target, int(status)))
    return out


def top3(text, offset_sign=1, keep_query=False, statuses=range(500, 600), inclusive=False):
    """Paths by errors in the window, most first.

    offset_sign 1 is right, 0 ignores the offset, -1 applies it backwards;
    inclusive counts a line at exactly 15:00:00 too.
    """
    w0 = WINDOW[0].replace(tzinfo=None)
    w1 = WINDOW[1].replace(tzinfo=None)
    counts = {}
    for local, offset, target, status in parse(text):
        utc = local - timedelta(minutes=offset_sign * offset)
        if status in statuses and w0 <= utc and (utc <= w1 if inclusive else utc < w1):
            key = target if keep_query else target.split("?", 1)[0]
            counts[key] = counts.get(key, 0) + 1
    ranked = sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))
    return ranked


def verify(text):
    ranked = top3(text)
    truth = ranked[:3]
    problems = []
    if len(ranked) < 4 or ranked[2][1] - ranked[3][1] < 2 or len({c for _, c in truth}) < 3:
        problems.append(f"top of the ranking isn't clear: {ranked[:5]}")
    if top3(text, inclusive=True) != ranked:
        problems.append("a line at exactly 15:00:00 makes the end of the hour matter")
    for name, kw in {
        "offset ignored": {"offset_sign": 0},
        "offset backwards": {"offset_sign": -1},
        "query kept": {"keep_query": True},
        "4xx counted": {"statuses": range(400, 600)},
    }.items():
        if top3(text, **kw)[:3] == truth:
            problems.append(f"'{name}' still gives the right answer")
    return truth, problems


def main():
    text = build()
    truth, problems = verify(text)
    if problems:
        sys.exit("; ".join(problems))
    out = Path(__file__).parent / "files" / "access.log"
    out.write_text(text, encoding="utf-8")
    print(f"{len(text.splitlines())} lines; top three: {truth}")
    for name, kw in {"offset ignored": {"offset_sign": 0}, "offset backwards": {"offset_sign": -1}, "query kept": {"keep_query": True}, "4xx counted": {"statuses": range(400, 600)}}.items():
        print(f"  {name}: {top3(text, **kw)[:4]}")


if __name__ == "__main__":
    main()
