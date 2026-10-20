"""Run in a scratch copy: every hidden booking against the agent's quote()."""

import json
import sys

sys.path.insert(0, ".")
from fares import quote  # noqa: E402

cases = json.load(open(sys.argv[1], encoding="utf-8"))
wrong = []
for i, case in enumerate(cases):
    want = case["quote"]
    try:
        got = quote(json.loads(json.dumps(case["booking"])))
        if "error" in want:
            wrong.append((i, "no error", "ValueError", case["booking"]))
            continue
        got_cmp = {k: got.get(k) for k in ("total", "legs", "promo_applied", "booking_fee")}
        if got_cmp != want:
            wrong.append((i, got_cmp, want, case["booking"]))
    except ValueError:
        if "error" not in want:
            wrong.append((i, "ValueError", want, case["booking"]))
    except Exception as e:  # noqa: BLE001
        wrong.append((i, f"{type(e).__name__}: {e}", want, case["booking"]))
print(json.dumps({"cases": len(cases), "wrong": len(wrong), "examples": [
    {"case": i, "booking": b, "got": g, "want": w} for i, g, w, b in wrong[:4]]}, ensure_ascii=False))
