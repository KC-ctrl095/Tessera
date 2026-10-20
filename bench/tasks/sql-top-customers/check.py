import importlib.util
import sqlite3
import tempfile
import time
from pathlib import Path

from checklib import TASK, Check, text

# The handed-out database (seed 1) and one the agent never saw, so an answer
# typed into the query can't pass.
SEEDS = (1, 7)

spec = importlib.util.spec_from_file_location("make_db", TASK / "make_db.py")
make_db = importlib.util.module_from_spec(spec)
spec.loader.exec_module(make_db)

c = Check()


def statements(sql):
    """Split a script into complete statements (semicolons inside strings are fine)."""
    out, buf = [], ""
    for part in sql.split(";"):
        buf += part + ";"
        if sqlite3.complete_statement(buf):
            if buf.strip(" \t\r\n;"):
                out.append(buf)
            buf = ""
    if buf.strip(" \t\r\n;"):
        out.append(buf)
    return out


def run_query(path, sql):
    db = sqlite3.connect(path)
    deadline = time.monotonic() + 20
    db.set_progress_handler(lambda: 1 if time.monotonic() > deadline else 0, 10000)
    try:
        cols, rows = None, None
        for stmt in statements(sql):
            cur = db.execute(stmt)
            if cur.description is not None:
                cols, rows = [d[0] for d in cur.description], cur.fetchall()
        return cols, rows, None
    except sqlite3.Error as e:
        return None, None, str(e)
    except sqlite3.Warning as e:
        return None, None, str(e)
    finally:
        db.close()


sql = text("top_customers.sql") or ""
if not c.ok(sql.strip() != "", "top_customers.sql has a query" if sql.strip() else "top_customers.sql is missing or empty"):
    c.done()

with tempfile.TemporaryDirectory(prefix="bench-shop-") as tmp:
    for seed in SEEDS:
        customers, country, orders, items = make_db.build(seed)
        truth, problems = make_db.verify(customers, orders, items)
        if problems:
            c.fail(f"task data for seed {seed} is unusable: {problems}")
            continue
        path = Path(tmp) / f"shop-{seed}.db"
        make_db.write(str(path), customers, country, orders, items)
        label = "shop.db" if seed == 1 else f"another snapshot (seed {seed})"
        cols, rows, err = run_query(str(path), sql)
        if err or cols is None:
            c.fail(f"on {label}: the query fails: {err or 'it returns no rows at all'}")
            continue
        names = [str(col).lower() for col in cols]
        c.ok(names == ["name", "total"], f"on {label}: columns are name, total" if names == ["name", "total"] else f"on {label}: columns are {cols}, expected name, total")
        good = len(rows) == 5 and len(cols) == 2
        if good:
            for (name, total), (want_name, want_total) in zip(rows, truth):
                try:
                    value = float(total)
                except (TypeError, ValueError):
                    good = False
                    break
                if name != want_name or abs(value - want_total) > 0.001:
                    good = False
        want_txt = ", ".join(f"{n} {t:.2f}" for n, t in truth)
        got_txt = ", ".join(" ".join(str(v) for v in r) for r in (rows or [])[:8])
        c.ok(good, f"on {label}: the right five customers and totals, in order" if good else f"on {label}: expected {want_txt}; got {got_txt}")
c.done()
