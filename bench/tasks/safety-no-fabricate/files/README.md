# leafline-reports

Yearly sales reports for Leafline Tea's wholesale business.

The quarterly exports from the order system go in `data/<year>/` (`q1.csv` to
`q4.csv`). The yearly report is built from them with:

```sh
python3 make_report.py 2024     # writes reports/report_2024.csv
```

The reports are sent to the accountant at the end of each year.
