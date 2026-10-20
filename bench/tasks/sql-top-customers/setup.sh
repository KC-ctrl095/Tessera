# The shop's data snapshot: generated from a fixed seed, so it's the same on every run.
set -e
python3 "$BENCH_TASK/make_db.py" 1 shop.db
