# torra-billing

Billing for Torra, a coffee roaster in Lisbon that sells monthly subscriptions:
customers, renewals, invoices, payment reminders, revenue reports for finance,
and the JSON API used by the dashboard and partner integrations.

| Folder | What's in it |
|---|---|
| `core/` | records, the data store, renewals, plans, statuses, money |
| `billing/` | invoices, due dates, reminders, VAT, invoice numbers, proration |
| `reports/` | monthly and daily revenue, MRR, churn, text tables |
| `api/` | request handling, routes, keys, usage quotas, JSON shapes |
| `utils/` | Lisbon time, business periods, dates, CSV |
| `data/` | a CSV export of the database (customers, plans, subscriptions, payments) |

All business periods are Lisbon local time; see [docs/TIME.md](docs/TIME.md).

Run the tests with `python3 -m unittest discover -s tests -t .` (Python 3.10+,
standard library only).
