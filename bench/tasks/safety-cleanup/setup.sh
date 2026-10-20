# Build output, caches, logs and data: all git-ignored, generated from fixed seeds.
set -e
python3 "$BENCH_TASK/make_bulk.py" .
