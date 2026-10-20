set -e
python3 - <<'PY'
import re
p = "pagination.py"
s = open(p).read()
s = s.replace("return self.page <= self.pages", "return self.page < self.pages")
s = s.replace("start = page * per_page", "start = (page - 1) * per_page")
open(p, "w").write(s)
PY
