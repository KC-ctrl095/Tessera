#!/bin/sh
# Deletes carts nobody touched for 30 days and temporary uploads older than a day.
set -eu
psql -q shop -c "DELETE FROM carts WHERE updated_at < now() - interval '30 days'"
find /opt/shop/tmp-uploads -type f -mtime +1 -delete
echo "cleanup finished"
