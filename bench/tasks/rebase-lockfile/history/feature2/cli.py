"""python3 -m ledger.cli EXPORT.csv [--monthly | --csv | --xlsx OUT.xlsx]"""

import argparse
import sys

from . import export, parse, report


def main(argv=None):
    ap = argparse.ArgumentParser(prog="ledger", description="Summarize a bank export.")
    ap.add_argument("export", help="the bank's CSV export")
    ap.add_argument("--monthly", action="store_true", help="totals per month instead of per category")
    ap.add_argument("--csv", action="store_true", help="the summary as CSV")
    ap.add_argument("--xlsx", metavar="OUT", help="write the summary to a spreadsheet (needs tabwrite)")
    args = ap.parse_args(argv)
    transactions = parse.read_file(args.export)
    if args.monthly:
        for month, total in report.monthly(transactions).items():
            print(f"{month}  {total:>10}")
    elif args.csv:
        sys.stdout.write(export.to_csv(report.summarize(transactions)))
    elif args.xlsx:
        export.to_xlsx(report.summarize(transactions), args.xlsx)
    else:
        print(report.render(report.summarize(transactions)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
