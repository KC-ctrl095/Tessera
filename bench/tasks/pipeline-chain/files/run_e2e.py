"""End-to-end check: build the September report and compare it with the signed-off one.

    python3 run_e2e.py

Prints the differences (expected vs. got) and exits 1 when they differ.
"""

import difflib
import subprocess
import sys

expected = open("expected/report.txt", encoding="utf-8").read().splitlines()
run = subprocess.run(
    [sys.executable, "-m", "tripstats", "data/trips.csv", "data/stations.json", "September 2026"],
    capture_output=True, text=True,
)
if run.returncode != 0:
    print(run.stderr)
    sys.exit(1)
got = run.stdout.splitlines()
diff = list(difflib.unified_diff(expected, got, "expected/report.txt", "report (got)", lineterm="", n=1))
if diff:
    print("\n".join(diff))
    changed = sum(1 for line in diff if line.startswith("+") and not line.startswith("+++"))
    print(f"\nThe report differs from expected/report.txt ({changed} lines).")
    sys.exit(1)
print("The report matches expected/report.txt.")
