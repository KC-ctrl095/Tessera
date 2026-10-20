# The two bugs behind the failing integration checks. (An agent finds them by
# starting the dev server in the background with its output in a log file,
# waiting for "ready", running tests/integration.py, and reading the server's
# traceback for the 500.)
set -e
python3 - <<'PY'
from pathlib import Path

def fix(path, old, new):
    p = Path(path)
    s = p.read_text()
    assert old in s, (path, old)
    p.write_text(s.replace(old, new))

# Page N has a next page only when items remain after it.
fix("parcelhub/pagination.py", "has_next=page * per_page <= total,", "has_next=page * per_page < total,")
# Link URLs must be percent-encoded (non-ASCII text can't go into an HTTP header as is).
fix("parcelhub/urls.py", '"""URLs the API hands out (Link headers)."""\n', '"""URLs the API hands out (Link headers)."""\n\nfrom urllib.parse import urlencode\n')
fix("parcelhub/urls.py", 'return path + "?" + "&".join(f"{name}={value}" for name, value in params.items())', 'return path + "?" + urlencode(params)')
PY
