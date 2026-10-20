#!/bin/sh
# backup.sh SRC DEST
#
# Makes one archive per folder directly inside SRC: DEST/<folder>-<date>.tar.gz.
# Each archive holds the folder itself (<folder>/...), so it unpacks into a
# folder of the same name. Plain files in SRC are not backed up.
# The date is BACKUP_DATE when it is set (YYYYMMDD), otherwise today's date.
#
# Runs every night from cron:  backup.sh /srv/files /mnt/nas/backups

SRC=$1
DEST=$2
DATE=${BACKUP_DATE:-$(date +%Y%m%d)}

if [ -z "$SRC" ] || [ -z "$DEST" ]; then
  echo "usage: backup.sh SRC DEST" >&2
  exit 2
fi

mkdir -p $DEST
cd $SRC
for d in $(ls); do
  if [ -d $d ]; then
    tar -czf $DEST/$d-$DATE.tar.gz $d
    echo "backed up $d"
  fi
done
echo "done"
