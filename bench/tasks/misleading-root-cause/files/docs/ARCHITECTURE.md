# Architecture

`core.store.Store` loads the data (a CSV export here; the database in
production). Everything else is plain functions over the store's records:

- `reports.monthly.monthly_report(store, year, month)` -> the month finance
  closes; `reports.daily`, `reports.mrr`, `reports.churn` feed the dashboard.
- `billing.invoices.invoice_for(store, payment, numbering)` builds an invoice:
  number (`billing.numbering`), VAT (`billing.tax`, `core.money.split_vat`) and
  due date (`billing.due_dates`). `billing.dunning` schedules the reminders.
- `api.handlers.Api.handle(method, path, headers, now)` serves the JSON API;
  `api.usage` counts calls per Lisbon day for the partner quotas.

Time handling is shared: `utils.tz` and `utils.periods` (see TIME.md).
