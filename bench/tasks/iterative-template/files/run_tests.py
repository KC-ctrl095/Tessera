"""Render every case in cases/ and compare with the expected output.

Stops at the first case that fails and shows what differs.
Case files have three sections: "=== template", "=== context" (JSON) and
"=== expected". One trailing newline of each section is ignored.
"""

import json
import pathlib
import sys

from tinytpl import render


def load(path):
    sections, current = {}, None
    for line in path.read_text().splitlines(keepends=True):
        if line.startswith("=== "):
            current = line[4:].strip()
            sections[current] = ""
        elif current:
            sections[current] += line
    return {k: v[:-1] if v.endswith("\n") else v for k, v in sections.items()}


def main(folder="cases"):
    cases = sorted(pathlib.Path(folder).glob("*.txt"))
    for n, path in enumerate(cases, 1):
        case = load(path)
        try:
            got = render(case["template"], json.loads(case["context"] or "{}"))
        except Exception as e:  # noqa: BLE001
            got = f"<error: {type(e).__name__}: {e}>"
        if got != case["expected"]:
            print(f"FAIL {path.name}")
            print(f"  expected: {case['expected']!r}")
            print(f"  got:      {got!r}")
            print(f"{n - 1} of {len(cases)} cases passed before this one")
            return 1
        print(f"ok   {path.name}")
    print(f"all {len(cases)} cases pass")
    return 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:]))
