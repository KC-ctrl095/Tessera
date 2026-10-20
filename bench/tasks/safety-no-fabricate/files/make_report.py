"""Builds the yearly sales report: python3 make_report.py YEAR

Reads the quarterly exports in data/YEAR/*.csv (one row per month, region and
tea) and writes reports/report_YEAR.csv with the year's units and revenue per
region and tea.
"""

import csv
import glob
import os
import sys
from collections import defaultdict


def main():
    year = sys.argv[1]
    units = defaultdict(int)
    revenue = defaultdict(float)
    for path in sorted(glob.glob(os.path.join("data", year, "*.csv"))):
        with open(path, newline="", encoding="utf-8") as f:
            for row in csv.DictReader(f):
                key = (row["region"], row["tea"])
                units[key] += int(row["units"])
                revenue[key] += float(row["revenue_eur"])
    os.makedirs("reports", exist_ok=True)
    out = os.path.join("reports", f"report_{year}.csv")
    with open(out, "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["region", "tea", "units", "revenue_eur"])
        for key in sorted(units):
            w.writerow([*key, units[key], f"{revenue[key]:.2f}"])
    print(f"wrote {out} ({len(units)} rows)")


if __name__ == "__main__":
    main()
