"""Builds shop.db for the sql-top-customers task: python3 make_db.py SEED OUT.db

The same seed always gives the same data. On top of random orders it plants
orders that catch the usual mistakes (wrong year bounds, refunded and
cancelled orders counted, integer division), and it refuses to write a
database where any of those mistakes would still give the right answer or
where the top five has a tie.
"""

import random
import sqlite3
import sys
from datetime import datetime, timedelta, timezone

FIRST = [
    "Ada", "Bram", "Carla", "Dries", "Elif", "Femke", "Goran", "Hanna", "Ilse", "Joost",
    "Kaja", "Lotte", "Mehmet", "Noor", "Oskar", "Pia", "Quinten", "Rosa", "Sven", "Tess",
    "Ulla", "Vera", "Wout", "Xander", "Yara", "Zeno", "Amira", "Bas", "Cato", "Daan",
]
LAST = [
    "Aalbers", "Brink", "Claes", "Dekker", "Eriksen", "Fontaine", "Grootveld", "Haas", "Ivanova", "Jansen",
    "Kowalski", "Lindqvist", "Mertens", "Nijland", "Ortega", "Peeters", "Quist", "Riedel", "Schouten", "Tamm",
    "Uysal", "Vos", "Willems", "Xu", "Yilmaz", "Zwart", "Bakker", "Coppens", "Durand", "Engel",
]
COUNTRIES = ["NL", "BE", "DE", "FR", "DK", "SE", "PT", "IE", "AT", "PL"]
PRODUCTS = [
    ("Pour-over kettle", 4290),
    ("Burr grinder", 12950),
    ("Ceramic dripper", 2399),
    ("Paper filters, 100", 649),
    ("Espresso machine", 89900),
    ("Milk jug", 1875),
    ("Coffee scale", 3445),
    ("Beans: Huila, 1 kg", 3120),
    ("Beans: Sidamo, 1 kg", 3385),
    ("Tamper", 2760),
    ("Travel mug", 2195),
    ("Descaler", 999),
    ("Knock box", 1640),
    ("Cold brew jar", 2850),
]
N_CUSTOMERS = 150
N_ORDERS = 2400
START = datetime(2024, 9, 1, tzinfo=timezone.utc)
END = datetime(2026, 3, 31, 23, 59, 59, tzinfo=timezone.utc)

SCHEMA = """
CREATE TABLE customers (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    country TEXT NOT NULL
);
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers(id),
    created_at TEXT NOT NULL,  -- ISO 8601, UTC
    status TEXT NOT NULL CHECK (status IN ('paid', 'refunded', 'cancelled'))
);
CREATE TABLE order_items (
    order_id INTEGER NOT NULL REFERENCES orders(id),
    product TEXT NOT NULL,
    qty INTEGER NOT NULL,
    unit_price_cents INTEGER NOT NULL
);
CREATE INDEX orders_customer ON orders(customer_id);
CREATE INDEX items_order ON order_items(order_id);
"""


def iso(t):
    return t.strftime("%Y-%m-%dT%H:%M:%SZ")


def in_2025(created_at):
    return datetime.fromisoformat(created_at.replace("Z", "+00:00")).year == 2025


def totals(orders, items, keep):
    """Cents per customer over the orders `keep(order)` accepts."""
    amount = {}
    for oid, qty, price in items:
        amount[oid] = amount.get(oid, 0) + qty * price
    out = {}
    for oid, cust, created, status in orders:
        if keep(created, status):
            out[cust] = out.get(cust, 0) + amount.get(oid, 0)
    return out


def top(customers, tot, n=5, divide=lambda c: c / 100):
    ranked = sorted(tot.items(), key=lambda kv: -kv[1])
    return [(customers[c], divide(v)) for c, v in ranked[:n]]


def truth_keep(created, status):
    return status == "paid" and in_2025(created)


def build(seed):
    rng = random.Random(seed)
    names = rng.sample([f"{f} {l}" for f in FIRST for l in LAST], N_CUSTOMERS)
    customers = {i + 1: names[i] for i in range(N_CUSTOMERS)}
    country = {i: rng.choice(COUNTRIES) for i in customers}
    weights = [rng.paretovariate(1.4) for _ in customers]
    span = int((END - START).total_seconds())
    orders, items = [], []  # orders: [key, customer, created_at, status]; items: [key, product, qty, price]

    def add_order(cust, created, status, lines):
        key = len(orders) + 1
        orders.append([key, cust, created, status])
        for product, qty, price in lines:
            items.append([key, product, qty, price])

    for _ in range(N_ORDERS):
        cust = rng.choices(list(customers), weights)[0]
        created = iso(START + timedelta(seconds=rng.randrange(span)))
        status = rng.choices(["paid", "refunded", "cancelled"], [82, 9, 9])[0]
        lines = []
        for _ in range(rng.randint(1, 4)):
            product, price = rng.choice(PRODUCTS)
            lines.append((product, rng.randint(1, 3), price))
        add_order(cust, created, status, lines)

    def cents():
        return totals([o for o in orders], [(k, q, p) for k, _, q, p in items], truth_keep)

    def big_lines(amount):
        """Order lines worth a bit more than amount cents."""
        machines = amount // 89900 + 1
        return [("Espresso machine", machines, 89900), ("Coffee scale", 1, 3445)]

    ranked = sorted(cents().items(), key=lambda kv: -kv[1])
    (c1, t1), (c2, t2), (c3, _), (c4, _), (c5, _), (c6, t6), (c7, t7), (c8, t8), (c9, t9) = ranked[:9]
    # Inside the year, at its very edges: a bound that is off by a second or
    # BETWEEN '2025-01-01' AND '2025-12-31' changes these customers' totals.
    add_order(c1, "2025-12-31T22:47:13Z", "paid", [("Burr grinder", 1, 12950), ("Milk jug", 2, 1875)])
    add_order(c3, "2025-01-01T00:00:00Z", "paid", [("Travel mug", 3, 2195)])
    add_order(c4, "2025-12-31T23:59:59Z", "paid", [("Knock box", 1, 1640), ("Descaler", 1, 999)])
    # Just outside the year: counting these lifts customers 6 and 7 to the top.
    add_order(c6, "2024-12-31T23:59:59Z", "paid", big_lines(t1 - t6))
    add_order(c7, "2026-01-01T00:00:00Z", "paid", big_lines(t1 - t7))
    # Big orders that never brought money in.
    add_order(c8, "2025-06-14T11:02:40Z", "refunded", big_lines(t1 - t8))
    add_order(c9, "2025-09-03T16:20:05Z", "cancelled", big_lines(t2 - t9))

    # Ids follow the order date, as they would in the shop.
    orders.sort(key=lambda o: (o[2], o[0]))
    new_id = {o[0]: i + 1 for i, o in enumerate(orders)}
    orders = [(new_id[k], c, t, s) for k, c, t, s in orders]
    items = [(new_id[k], p, q, pr) for k, p, q, pr in items]
    return customers, country, orders, items


def verify(customers, orders, items):
    """The planted mistakes must all give a wrong answer, and the top five must be clear."""
    flat = [(o, q, p) for o, _, q, p in items]
    right = totals(orders, flat, truth_keep)
    ranked = sorted(right.values(), reverse=True)
    problems = []
    if len(set(ranked[:6])) < 6:
        problems.append("tie in the top six")
    truth = top(customers, right)
    if all(round(v * 100) % 100 == 0 for _, v in truth):
        problems.append("no cents in the top five totals")
    wrong = {
        "any status": lambda c, s: in_2025(c),
        "no year": lambda c, s: s == "paid",
        "BETWEEN dates": lambda c, s: s == "paid" and "2025-01-01" <= c <= "2025-12-31",
        "after first second": lambda c, s: s == "paid" and "2025-01-01T00:00:00Z" < c < "2026-01-01T00:00:00Z",
        "ends a second early": lambda c, s: s == "paid" and "2025-01-01T00:00:00Z" <= c < "2025-12-31T23:59:59Z",
        "2024-12-31 included": lambda c, s: s == "paid" and "2024-12-31" <= c < "2026-01-01",
        "2026-01-01 included": lambda c, s: s == "paid" and "2025-01-01" <= c <= "2026-01-01T00:00:00Z",
        "not cancelled": lambda c, s: s != "cancelled" and in_2025(c),
        "not refunded": lambda c, s: s != "refunded" and in_2025(c),
    }
    for name, keep in wrong.items():
        if top(customers, totals(orders, flat, keep)) == truth:
            problems.append(f"'{name}' still gives the right answer")
    if top(customers, right, divide=lambda c: c // 100) == truth:
        problems.append("integer division still gives the right answer")
    return truth, problems


def write(path, customers, country, orders, items):
    db = sqlite3.connect(path)
    db.executescript(SCHEMA)
    db.executemany("INSERT INTO customers VALUES (?, ?, ?)", [(i, n, country[i]) for i, n in customers.items()])
    db.executemany("INSERT INTO orders VALUES (?, ?, ?, ?)", orders)
    db.executemany("INSERT INTO order_items VALUES (?, ?, ?, ?)", items)
    db.commit()
    db.close()


def main():
    seed, out = int(sys.argv[1]), sys.argv[2]
    customers, country, orders, items = build(seed)
    truth, problems = verify(customers, orders, items)
    if problems:
        sys.exit(f"seed {seed} makes a poor database: {'; '.join(problems)}")
    write(out, customers, country, orders, items)


if __name__ == "__main__":
    main()
