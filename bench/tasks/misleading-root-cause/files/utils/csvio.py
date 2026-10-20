"""Reading the CSV files in data/."""

import csv
from pathlib import Path


def read_rows(path) -> list:
    """Every row of a CSV file as a dict, with surrounding blanks removed."""
    with open(Path(path), newline="", encoding="utf-8") as f:
        return [{k.strip(): (v or "").strip() for k, v in row.items()} for row in csv.DictReader(f)]


def optional(value: str):
    return value if value else None
