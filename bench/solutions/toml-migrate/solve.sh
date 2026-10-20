set -e
python3 - <<'PY'
p = "settings.toml"
s = open(p).read()
reps = [
    ("# Changes take effect after a restart.\n", "# Changes take effect after a restart.\n\nconfig_version = 3\n"),
    ("# Address and port to listen on.\nlisten = ", "# Address and port to listen on.\nbind = "),
    ("# How long a request may take before the server gives up, in seconds.\ntimeout = 45\n",
     "# How long a request may take before the server gives up, in milliseconds.\ntimeout_ms = 45000\n"),
    ('# Where the log goes; it is rotated every night.\nfile = "/var/log/ledger/api.log"\n',
     '\n[log.file]\n# Where the log goes; it is rotated every night.\npath = "/var/log/ledger/api.log"\nkeep_days = 14\n'),
]
for old, new in reps:
    assert s.count(old) == 1, old
    s = s.replace(old, new)
open(p, "w").write(s)
PY
