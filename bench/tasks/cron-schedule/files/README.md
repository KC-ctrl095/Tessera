# Shop server: operations

The shop runs on a single server. This repository holds the maintenance scripts
and the files that set up their schedule.

## Layout on the server

- `scripts/` is deployed to `/opt/shop/scripts/` (the scripts are executable).
- Logs of the maintenance jobs go to `/var/log/shop/`, one file per job.
  `deploy/logrotate.conf` rotates them weekly.
- Everything runs as the `shop` user.

## Scripts

| script           | what it does                                              |
|------------------|-----------------------------------------------------------|
| `backup.sh`      | dumps the database and uploads it to the backup bucket    |
| `cleanup.sh`     | deletes expired carts and temporary uploads               |
| `report.py`      | emails the daily sales summary to the office              |
| `healthcheck.sh` | pings the storefront and the API, alerts when one is down |

## Installing the schedule

The schedule is a user crontab in `deploy/crontab`. On the server:

    crontab -u shop deploy/crontab
