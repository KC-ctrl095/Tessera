#!/bin/sh
# Copies the uploads folder into a dated backup archive.
# Usage: bin/backup.sh [target-folder]
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
. "$here/config/app.conf"

target=${1:-$BACKUP_DIR}
stamp=$(date +%Y%m%d-%H%M)
mkdir -p "$target"
tar -czf "$target/uploads-$stamp.tar.gz" -C "$APP_DIR" uploads
echo "backup written to $target/uploads-$stamp.tar.gz"
