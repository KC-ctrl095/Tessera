set -e
# Every module that used legacy_db, moved to db.Repo (see the comments in MIGRATION.md's special cases).
cp "$BENCH_SOLUTION"/clinic/*.py clinic/
rm legacy_db.py
