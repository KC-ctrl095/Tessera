"""The test suite, slowed down to what the real one takes on CI."""

import sys
import time

sys.path.insert(0, "build/app")
from readlist.core import parse_line, reading_time  # noqa: E402

CASES = [
    ("Dune | Frank Herbert | 412", ("Dune", "Frank Herbert", 412)),
    ("Kindred|Octavia E. Butler|264", ("Kindred", "Octavia E. Butler", 264)),
    ("  The Dispossessed | Ursula K. Le Guin | 387 ", ("The Dispossessed", "Ursula K. Le Guin", 387)),
    ("Either|Or | Soren Kierkegaard | 752", ("Either|Or", "Soren Kierkegaard", 752)),
]
failures = 0
for n, (line, want) in enumerate(CASES * 6, 1):
    try:
        got = parse_line(line)
    except Exception as e:  # noqa: BLE001
        got = f"{type(e).__name__}: {e}"
    ok = got == want
    failures += not ok
    print(f"    test {n:02d} parse_line {'ok' if ok else 'FAILED: ' + repr(got)}")
    time.sleep(0.35)
for pages, hours in [(300, 7.5), (40, 1.0), (0, 0.0)]:
    got = reading_time(pages)
    ok = abs(got - hours) < 1e-9
    failures += not ok
    print(f"    test reading_time({pages}) {'ok' if ok else 'FAILED: ' + repr(got)}")
print(f"    {len(CASES) * 6 + 3 - failures} passed, {failures} failed")
sys.exit(1 if failures else 0)
