# Upgrading the settings file

ledger-api refuses to start with a settings file in an older format and tells
you which version it found. Upgrade the file by hand as described below.

## Version 2 to version 3

Files without a `config_version` key are version 2.

1. Add `config_version = 3` at the top of the file, before the first section.
2. In `[server]`, `listen` is now called `bind`. The value stays the same.
3. In `[server]`, `timeout` in seconds is replaced by `timeout_ms` in
   milliseconds: `timeout = 2` becomes `timeout_ms = 2000`.
4. In `[log]`, `file = "<path>"` becomes a table of its own, `[log.file]`, with
   `path = "<path>"` and `keep_days`, the number of days of rotated log files
   to keep. Version 2 always kept 14 days: write `keep_days = 14` to keep that.

   ```toml
   [log]
   level = "warn"

   [log.file]
   path = "/var/log/app.log"
   keep_days = 14
   ```

Nothing else changes.

## Version 1 to version 2

1. The `[db]` section is now called `[database]`.
2. `[cache] size` is now `[cache] max_entries`.
