"""Reading CSV imports (customers, products)."""

import csv


def read_rows(path):
    with open(path, newline="", encoding="utf-8-sig") as f:
        return [{k.strip().lower(): (v or "").strip() for k, v in row.items()} for row in csv.DictReader(f)]
