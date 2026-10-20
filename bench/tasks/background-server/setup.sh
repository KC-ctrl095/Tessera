# A free local port for this run's dev server, written to config/dev.json (git-ignored).
set -e
python3 - <<'PY'
import json, socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
port = s.getsockname()[1]
s.close()
cfg = json.load(open("config/dev.json.example"))
cfg["port"] = port
with open("config/dev.json", "w") as f:
    json.dump(cfg, f, indent=2)
    f.write("\n")
PY
