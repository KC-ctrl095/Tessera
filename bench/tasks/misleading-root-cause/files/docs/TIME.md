# Time in torra-billing

- **Storage:** every instant (payments, sign-ups, cancellations, invoices) is a
  naive UTC `datetime`. The CSV export writes them as `2026-03-01T09:30:00Z`.
- **Business time:** customers, finance and support all work in Lisbon time.
  A business day, week or month starts at Lisbon midnight. Renewals, due dates
  and reminders are set in Lisbon wall-clock time.
- **Rules (mainland Portugal):** standard time is UTC+0. Summer time is UTC+1,
  from 01:00 UTC on the last Sunday of March to 01:00 UTC on the last Sunday of
  October. So one day in March lasts 23 hours and one day in October 25 hours.
- **Code:** `utils/tz.py` converts between UTC and Lisbon time. It's written
  out by hand because tzdata isn't installed on every machine the service runs
  on (the slim containers and the old NAS that runs the nightly exports).
  `utils/periods.py` turns Lisbon days, weeks and months into UTC windows
  `[start, end)`; use those rather than doing offset arithmetic by hand.
