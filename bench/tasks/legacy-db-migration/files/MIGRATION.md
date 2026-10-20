# Moving from legacy_db to db.Repo

`legacy_db.run()` hands out tuples, takes its parameters as `*args`, and
leaves transactions to raw `BEGIN`/`COMMIT` strings. `db.Repo` (in `db/`)
replaces it. It is finished, reviewed and tested (`tests/test_repo.py`):
don't change `db/`. Move every caller over, then delete `legacy_db.py`.

## Getting the repo

`app.setup()` opens the database with `db.connect(path)` (instead of
`legacy_db.connect(path)`) and creates the schema with `db.repo().script(SCHEMA)`.
Code that needs the database calls `db.repo()` when it runs (not at import
time: every `setup()` opens a new one).

## Calls

| legacy_db | db.Repo |
|---|---|
| `connect(path)` | `db.connect(path)` |
| `connection().executescript(sql)` | `repo.script(sql)` |
| `run(sql, *args)` or `run(sql, *args, fetch="all")` | `repo.all(sql, params)`: a list of `Row` |
| `run(sql, *args, fetch="one")` | `repo.one(sql, params)` or `repo.one_or_none(sql, params)`: see special case 2 |
| `run(sql, *args, fetch=None)` | `repo.execute(sql, params)`: the number of rows changed |
| `run(sql, *args, fetch="id")` | `repo.insert(sql, params)`: the id of the new row |
| `run("BEGIN")` ... `run("COMMIT")` / `run("ROLLBACK")` | `with repo.transaction():` (special case 4) |

There is also `repo.scalar(sql, params)` for queries that return a single
value (`COUNT(*)`, `SUM(...)`, `MAX(...)`).

**Parameters** are one tuple, list or dict, never loose values:
`run(sql, a, b)` becomes `repo.all(sql, (a, b))`, `run(sql, x)` becomes
`repo.all(sql, (x,))`, and `run(sql, *ids)` becomes `repo.all(sql, ids)`.
A bare value is refused with a TypeError.

**Rows** are `Row` objects: read a column as `row.name` or `row["name"]`,
`row.as_dict()` gives a dict. A Row is not a tuple: `row[0]`, unpacking
(`a, b = row`, `for a, b in rows`) and iterating over a row raise TypeError,
so code no longer depends on the order of the SELECT.

**Errors:** `db.NoRow`, `db.TooManyRows`, `db.AmbiguousColumns`,
`db.UseTransaction`; `db.IntegrityError` is sqlite3's, for constraint
violations (duplicate keys...).

## Special cases

1. **Tuple indexes and unpacking.** Code that indexes (`row[2]`, `rows[0][0]`)
   or unpacks rows must read the columns by the names its SELECT gives them.
   Name computed columns: `SELECT COUNT(*) AS n ...` and then `row.n` (or use
   `repo.scalar()`). Code that hands rows to something expecting sequences
   (`dict(rows)`, `csv.writer.writerows(rows)`, `" / ".join(row)`) must build
   those values from the columns.
2. **`fetch="one"`.** `repo.one()` wants exactly one row: it raises `NoRow`
   when nothing matches and `TooManyRows` when several rows do.
   - Callers that handle "nothing found" (`if row is None`, `if not row`,
     `row[0] if row else None`) use `repo.one_or_none()`, which returns None.
   - `fetch="one"` used to give the first of several rows. Where a query can
     match several rows and the caller wants the first (the next free slot,
     the latest note...), say so in the SQL with `LIMIT 1` after the
     `ORDER BY`; `one()`/`one_or_none()` would raise otherwise.
3. **`SELECT *` and duplicate names.** List the columns you use instead of
   `SELECT *`. A result can't have two columns with the same name (a join's
   `p.name` and `pr.name`, `SELECT *` over a join): Repo raises
   `AmbiguousColumns`, so give them distinct names with `AS`. The old
   `row[8]`-style indexes into `SELECT *` results follow the column order of
   the tables in `clinic/schema.py`.
4. **Transactions.** `repo.execute("BEGIN")` (and COMMIT/ROLLBACK) is
   refused. Put the statements that were between `BEGIN` and `COMMIT` in a
   `with repo.transaction():` block: it commits at the end of the block and
   rolls everything back if anything inside raises, so the old
   `try/except: ROLLBACK; raise` goes away. Blocks can be nested.
