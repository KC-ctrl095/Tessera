"""python3 -m ledger.cli EXPORT.csv [--monthly]"""

import argparse
import sys

from . import parse, report


def main(argv=None):
    ap = argparse.ArgumentParser(prog="ledger", description="Summarize a bank export.")
    ap.add_argument("export", help="the bank's CSV export")
    ap.add_argument("--monthly", action="store_true", help="totals per month instead of per category")
    args = ap.parse_args(argv)
    transactions = parse.read_file(args.export)
    if args.monthly:
        for month, total in report.monthly(transactions).items():
            print(f"{month}  {total:>10}")
    else:
        print(report.render(report.summarize(transactions)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
