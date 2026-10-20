#!/bin/sh
# Dumps the shop database and uploads the dump to the backup bucket.
set -eu
stamp=$(date +%Y%m%d-%H%M)
dump=/var/tmp/shop-$stamp.sql.gz
pg_dump --no-owner shop | gzip > "$dump"
rclone copy "$dump" backups:shop-db/
rm -f "$dump"
echo "backup $stamp uploaded"
