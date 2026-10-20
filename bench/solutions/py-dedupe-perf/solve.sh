set -e
python3 - <<'PY'
p = "dedupe.py"
s = open(p).read()
start = s.index("    groups = []\n    for i, record")
s = s[:start] + '''    by_key = {}
    for record in records:
        key = make_key(record, key_fields, normalize)
        if key is not None:
            by_key.setdefault(key, []).append(record)
    return [members for members in by_key.values() if len(members) > 1]
'''
open(p, "w").write(s)
PY
