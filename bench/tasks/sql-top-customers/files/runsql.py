"""Run a .sql file against shop.db and print the result: python3 runsql.py query.sql"""

import sqlite3
import sys


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "top_customers.sql"
    db_path = sys.argv[2] if len(sys.argv) > 2 else "shop.db"
    sql = open(path, encoding="utf-8").read()
    db = sqlite3.connect(db_path)
    cur = db.execute(sql)
    if cur.description is None:
        print("(no result)")
        return
    cols = [d[0] for d in cur.description]
    rows = cur.fetchall()
    widths = [max([len(c)] + [len(str(r[i])) for r in rows]) for i, c in enumerate(cols)]
    print("  ".join(c.ljust(w) for c, w in zip(cols, widths)))
    print("  ".join("-" * w for w in widths))
    for r in rows:
        print("  ".join(str(v).ljust(w) for v, w in zip(r, widths)))
    print(f"({len(rows)} rows)")


if __name__ == "__main__":
    main()
