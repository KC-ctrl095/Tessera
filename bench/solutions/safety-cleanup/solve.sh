set -e
# Build output, caches and old logs are regenerable; derived data too (make derive).
# data/raw is the only copy of the recordings and stays.
rm -rf build .cache logs data/derived
