# Log rotation

`rotate_logs.sh`, in the project root, rotates the app's log files. Cron runs it
once a day on our Linux servers (GNU coreutils and gzip are there):

    rotate_logs.sh DIR KEEP

## What it does

1. **Rotate.** For every regular file directly in DIR (not in its subfolders)
   whose name ends in `.log` and whose size is above zero:
   - Write a gzip-compressed copy of it into DIR, named
     `<name>.log.<YYYYMMDD>.gz`. The date is the log file's last modification
     time **in UTC**, whatever the server's time zone is. Example: `app.log`,
     last written at 2026-03-05 21:10 UTC, becomes `app.log.20260305.gz`.
   - If an archive with that name exists already, don't overwrite it: use
     `<name>.log.<YYYYMMDD>-2.gz`, or `-3`, `-4`, ... (the first free number).
   - Then empty the log file (truncate it to 0 bytes). Don't delete, move or
     recreate it: the app keeps the file open and goes on writing to it.

2. **Prune.** Then, for every `.log` file in DIR (also the ones that were empty
   and not rotated this time), keep only the KEEP newest archives of that log
   and delete the older ones.
   - "Newest" goes by the date in the archive's name, not by the file's
     modification time. For the same date, `-2` is newer than no number,
     `-3` is newer than `-2`, and so on.
   - An archive of `app.log` is a file named `app.log.<8 digits>.gz` or
     `app.log.<8 digits>-<number>.gz`. Nothing else counts: files such as
     `app.log.bak` or `app.log.old.gz` are left alone.

3. Everything else in DIR is left alone: empty logs (no archive, not touched),
   other files, subfolders and what is in them.

Log names can contain spaces (`web server.log`), and so can DIR.

## Wrong usage

When an argument is missing, KEEP isn't a positive whole number (1, 2, 3, ...),
or DIR isn't a folder, the script prints a usage line to stderr, such as

    usage: rotate_logs.sh DIR KEEP

and exits with status 2 without changing anything.

Otherwise it exits with status 0 when everything worked.
