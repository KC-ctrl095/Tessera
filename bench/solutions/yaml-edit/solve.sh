set -e
python3 - <<'PY'
p = "deploy/compose.yaml"
s = open(p).read()
old = """      # One worker handles about 40 jobs a minute.
      replicas: 1"""
assert old in s
s = s.replace(old, old.replace("replicas: 1", "replicas: 3"))
old = """      - SLOW_QUERY_MS=500
      - LOG_FORMAT=json
"""
assert s.count(old) == 1
s = s.replace(old, old + "      - LOG_LEVEL=debug\n")
open(p, "w").write(s)
PY
