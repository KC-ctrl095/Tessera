# shop-reports

SQL reports for the Brewbench web shop. The data is a snapshot of the shop's
database in `shop.db` (SQLite; not in git, it's refreshed every night). The
tables are described in `schema.sql`. Amounts are stored in cents; order times
are UTC.

There is no `sqlite3` command on the reporting box; run a query file with:

```sh
python3 runsql.py some_report.sql
```
