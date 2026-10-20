# dockyard

Order routing and fulfilment for our five warehouses (Amsterdam, Rotterdam,
Eindhoven, Antwerp, Cologne): which warehouse ships an order, how it's packed,
which carrier takes each parcel, when we promise delivery, and the invoice.

```
dockyard/
  catalog.py    the product catalogue (SKUs, prices, weights, sizes)
  inventory.py  stock per warehouse and reservations
  geo.py        rough locations from postcodes
  routing.py    which warehouse ships what
  packing.py    boxes
  rates.py      carrier rate cards
  carriers.py   carrier choice (live price APIs with the rate cards as fallback)
  schedule.py   shipping days, cut-offs, public holidays, delivery promises
  invoice.py    invoices with VAT
  pipeline.py   the whole flow for an order, and day simulations
  events.py     event bus and audit log
  store.py      shipment store (in-memory stand-in for the database)
  fixtures.py   sample data for tests and demos
  data/         public holidays per country
```

## Tests

```sh
python3 run_tests.py              # what CI runs: every log line at DEBUG, warnings on
python3 run_tests.py schedule     # only tests whose id contains "schedule"
```

The tests are plain `unittest` tests in `tests/`.

## 2.0 migration notes

`units.lbs_to_kg` and `units.mm_to_cm` are deprecated in favour of
`units.convert_mass` and `units.volume_cm3`; they still work but warn. They go
away in 3.0.
