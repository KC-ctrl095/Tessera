# ledgerline

Summaries of bank exports: totals per category and per month.

```sh
python3 -m ledger.cli export.csv            # totals per category
python3 -m ledger.cli export.csv --monthly  # totals per month
python3 -m unittest discover -s tests -t .  # tests
```

## Dependencies

`requirements.in` lists what we depend on; `deps.lock` pins exact versions
and is **generated**: after changing `requirements.in` (or refreshing the
package index snapshot in `tools/index.json`), run

```sh
python3 tools/lock.py
```

and commit `deps.lock` together with the change. Never edit `deps.lock` by
hand. CI runs `python3 tools/lock.py --check` on every commit (not just the
last one of a branch), so each commit must carry a `deps.lock` that matches
its own `requirements.in` and index.
