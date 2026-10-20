"""Exporting summaries: CSV always, XLSX when tabwrite is installed."""

import csv
import io


def to_csv(rows):
    """The summary rows as CSV text: category, total (2 decimals), count."""
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    w.writerow(["category", "total", "count"])
    for r in rows:
        w.writerow([r["category"], f"{r['total']:.2f}", r["count"]])
    return buf.getvalue()


def to_xlsx(rows, path):
    """Write the summary as a spreadsheet (needs the optional tabwrite package)."""
    try:
        import tabwrite
    except ImportError:
        raise RuntimeError("XLSX export needs tabwrite (pip install -r requirements.in)") from None
    with tabwrite.Workbook(path) as wb:
        sheet = wb.sheet("Summary")
        sheet.append(["category", "total", "count"])
        for r in rows:
            sheet.append([r["category"], float(r["total"]), r["count"]])
