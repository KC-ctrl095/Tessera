set -e
python3 - <<'PY'
p = "config/services.json"
s = open(p).read()
old = '''      "url": "https://search.internal:9200",
      "timeout_ms": 8000,'''
assert old in s
s = s.replace(old, old.replace("8000", "15000"))
anchor = '''      "tags": ["batch", "internal"]
    }
'''
assert anchor in s
s = s.replace(anchor, anchor.rstrip("\n") + ''',
    {
      "name": "billing",
      "url": "https://billing.internal:8443",
      "timeout_ms": 5000,
      "retries": 3,
      "tags": ["payments"]
    }
''')
open(p, "w").write(s)
PY
