#!/bin/sh
# Installs the daily maintenance jobs for the ledger user.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
crontab -u ledger - <<CRON
# Rotate the app logs at 00:05 and keep two weeks of archives.
5 0 * * * $here/rotate_logs.sh /var/log/ledger 14 >> /var/log/ledger-ops.txt 2>&1
CRON
echo "cron jobs installed"
