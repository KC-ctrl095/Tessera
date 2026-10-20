# The copy arrives with the modes the files had when they were uploaded:
# readable by everyone and nothing executable.
set -e
chmod 755 . bin config secrets
chmod 644 bin/deploy.sh bin/backup.sh bin/README config/app.conf notes.txt
chmod 644 secrets/api.key secrets/db.env
