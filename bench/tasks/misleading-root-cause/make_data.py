"""Generates files/data/*.csv for the misleading-root-cause task (fixed seed).

Payments are placed with the correct Lisbon rules (implemented here on their
own, and compared with zoneinfo when it's available), so the data is what a
correct service recorded. Run: python3 make_data.py
"""

import calendar
import csv
import random
from datetime import date, datetime, time, timedelta
from pathlib import Path

OUT = Path(__file__).resolve().parent / "files" / "data"
rng = random.Random(20261025)


def last_sunday(y, m):
    last = date(y, m, calendar.monthrange(y, m)[1])
    return last - timedelta(days=(last.weekday() + 1) % 7)


def offset(utc):
    start = datetime.combine(last_sunday(utc.year, 3), time(1))
    end = datetime.combine(last_sunday(utc.year, 10), time(1))
    return timedelta(hours=1) if start <= utc < end else timedelta(0)


def to_utc(local):
    s = local - timedelta(hours=1)
    if offset(s) == timedelta(hours=1):
        return s
    return local


def check_with_zoneinfo():
    try:
        from datetime import timezone
        from zoneinfo import ZoneInfo

        z = ZoneInfo("Europe/Lisbon")
    except Exception:
        print("zoneinfo not available; skipped the cross-check")
        return
    t = datetime(2024, 12, 31, 0, 0)
    while t < datetime(2029, 1, 2):
        real = t.replace(tzinfo=timezone.utc).astimezone(z).utcoffset()
        assert real == offset(t), (t, real, offset(t))
        t += timedelta(minutes=30)
    print("offsets agree with zoneinfo 2025-2028")


FIRST = ["Inês", "Tiago", "Mariana", "Rui", "Beatriz", "Duarte", "Leonor", "Gonçalo", "Carolina", "Afonso",
         "Matilde", "Rodrigo", "Sofia", "Tomás", "Joana", "Henrique", "Clara", "Vasco", "Lúcia", "Martim",
         "Elena", "Pablo", "Margot", "Jonas", "Saoirse", "Bram"]
LAST = ["Ferreira", "Lopes", "Carvalho", "Moreira", "Antunes", "Pires", "Nogueira", "Teixeira", "Barros",
        "Quintela", "Valente", "Serrano", "Amaral", "Rebelo", "Cardoso", "Fontes"]
BUSINESSES = {3: "Casa do Grão", 11: "Office Hub Porto", 27: "Roastery Lab Braga", 35: "Cowork Alfama"}
PLANS = [("espresso", "Espresso (1 bag)", 1290, 1), ("filter", "Filter (2 bags)", 2190, 2),
         ("sampler", "Sampler (3 bags)", 2890, 3), ("office", "Office (6 bags)", 5490, 6)]
PRICE = {p[0]: p[2] for p in PLANS}


def main():
    check_with_zoneinfo()
    OUT.mkdir(parents=True, exist_ok=True)
    customers = []
    for cid in range(1, 49):
        if cid in BUSINESSES:
            name = BUSINESSES[cid]
            email = "orders@" + name.lower().replace(" ", "").replace("ã", "a") + ".example"
            country = "PT"
        else:
            name = f"{rng.choice(FIRST)} {rng.choice(LAST)}"
            email = name.lower().replace(" ", ".").translate(str.maketrans("êçáúéãõ", "ecaueao")) + f"{cid}@mail.example"
            country = rng.choices(["PT", "ES", "FR", "DE", "NL", "IE", "BE"], [70, 8, 6, 5, 4, 4, 3])[0]
        customers.append((cid, name, email, country))

    end_of_data = to_utc(datetime(2029, 1, 1))
    subs = []

    def add_sub(cid, plan, start_month, day, hhmm, cancel_after_months=None):
        y, m = start_month
        local = datetime.combine(date(y, m, min(day, calendar.monthrange(y, m)[1])), hhmm)
        started = to_utc(local)
        cancelled = None
        if cancel_after_months is not None:
            idx = y * 12 + m - 1 + cancel_after_months
            cy, cm = idx // 12, idx % 12 + 1
            cday = rng.randint(1, 28)
            cancelled = to_utc(datetime(cy, cm, cday, rng.randint(7, 22), rng.randint(0, 59)))
        subs.append({"id": len(subs) + 1, "customer_id": cid, "plan": plan, "started_at": started,
                     "renewal_day": day, "renewal_time": hhmm, "cancelled_at": cancelled})

    # The renewals that sit next to month boundaries (late evening on the 30th/31st,
    # just after midnight on the 1st) make the monthly totals sensitive to the offset.
    add_sub(3, "office", (2025, 1), 31, time(23, 40))
    add_sub(11, "office", (2025, 3), 31, time(23, 5))
    add_sub(27, "sampler", (2025, 2), 1, time(0, 20))
    add_sub(35, "office", (2025, 5), 1, time(0, 45))
    add_sub(8, "filter", (2025, 6), 30, time(23, 55))
    add_sub(19, "espresso", (2025, 4), 28, time(23, 30), cancel_after_months=30)
    for cid in range(1, 49):
        for _ in range(rng.choice([1, 1, 1, 2])):
            if len(subs) >= 66:
                break
            start = (rng.randint(2024, 2028), rng.randint(1, 12))
            if start < (2024, 10) or start > (2028, 9):
                start = (2025, rng.randint(1, 12))
            hour = rng.choice([0, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23])
            day = rng.choice(list(range(1, 32)) + [1, 15, 28, 30, 31])
            cancel = rng.choice([None, None, None, rng.randint(2, 30)])
            add_sub(cid, rng.choice(["espresso", "espresso", "filter", "filter", "sampler", "office"]), start, day,
                    time(hour, rng.randint(0, 59)), cancel)

    payments = []
    for s in subs:
        y, m = s["started_at"].year, s["started_at"].month
        local_start = s["started_at"] + offset(s["started_at"])
        y, m = local_start.year, local_start.month
        while (y, m) <= (2028, 12):
            local = datetime.combine(date(y, m, min(s["renewal_day"], calendar.monthrange(y, m)[1])), s["renewal_time"])
            at = to_utc(local) + timedelta(seconds=rng.randint(0, 59))
            if at >= s["started_at"] and (s["cancelled_at"] is None or at < s["cancelled_at"]) and at < end_of_data:
                payments.append([s["id"], s["customer_id"], s["plan"], PRICE[s["plan"]], at, "renewal"])
            m += 1
            if m == 13:
                y, m = y + 1, 1
    renewals = list(payments)
    lo, hi = datetime(2025, 1, 1), datetime(2028, 12, 31)
    for _ in range(380):
        s = rng.choice(subs)
        a = max(s["started_at"], lo)
        b = min(s["cancelled_at"] or hi, hi)
        if b <= a:
            continue
        at = a + timedelta(seconds=rng.randint(0, int((b - a).total_seconds())))
        payments.append([s["id"], s["customer_id"], s["plan"], rng.choice([690, 890, 1290, 2490, 3490]), at, "extra"])
    # A few late-evening extras on the last days of October and March.
    for (y, mo, d, hh, mm) in [(2025, 10, 31, 23, 12), (2026, 10, 31, 23, 48), (2027, 10, 31, 23, 31),
                               (2028, 10, 31, 23, 6), (2026, 3, 31, 23, 25), (2027, 4, 1, 0, 33),
                               (2026, 11, 1, 0, 14), (2028, 4, 1, 0, 51), (2025, 4, 1, 0, 8)]:
        at = to_utc(datetime(y, mo, d, hh, mm, rng.randint(0, 59)))
        s = next(x for x in subs if x["customer_id"] == 3)
        payments.append([s["id"], 3, s["plan"], 1290, at, "extra"])
    for _ in range(34):
        r = rng.choice(renewals)
        at = r[4] + timedelta(days=rng.randint(1, 9), seconds=rng.randint(0, 86399))
        if at < end_of_data:
            payments.append([r[0], r[1], r[2], -r[3], at, "refund"])

    payments.sort(key=lambda p: (p[4], p[0], p[5]))
    with open(OUT / "customers.csv", "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(["id", "name", "email", "country"])
        w.writerows(customers)
    with open(OUT / "plans.csv", "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(["code", "name", "price_cents", "bags_per_month"])
        w.writerows(PLANS)
    with open(OUT / "subscriptions.csv", "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(["id", "customer_id", "plan", "started_at", "renewal_day", "renewal_time", "cancelled_at"])
        for s in subs:
            w.writerow([s["id"], s["customer_id"], s["plan"], s["started_at"].strftime("%Y-%m-%dT%H:%M:%SZ"),
                        s["renewal_day"], s["renewal_time"].strftime("%H:%M"),
                        s["cancelled_at"].strftime("%Y-%m-%dT%H:%M:%SZ") if s["cancelled_at"] else ""])
    with open(OUT / "payments.csv", "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(["id", "subscription_id", "customer_id", "plan", "amount_cents", "paid_at", "kind"])
        for n, p in enumerate(payments, 1001):
            w.writerow([n, p[0], p[1], p[2], p[3], p[4].strftime("%Y-%m-%dT%H:%M:%SZ"), p[5]])
    print(f"{len(customers)} customers, {len(subs)} subscriptions, {len(payments)} payments")


if __name__ == "__main__":
    main()
