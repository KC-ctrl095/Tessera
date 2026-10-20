set -e
cat > deploy/crontab <<'CRON'
# Maintenance jobs of the shop server (user crontab of `shop`).
# m  h  dom mon dow  command
30 2  *   *   *    /opt/shop/scripts/backup.sh >> /var/log/shop/backup.log 2>&1
0  4  *   *   0    /opt/shop/scripts/cleanup.sh >> /var/log/shop/cleanup.log 2>&1
15 18 *   *   1-5  python3 /opt/shop/scripts/report.py >> /var/log/shop/report.log 2>&1
*/5 * *   *   *    /opt/shop/scripts/healthcheck.sh >> /var/log/shop/healthcheck.log 2>&1
CRON
