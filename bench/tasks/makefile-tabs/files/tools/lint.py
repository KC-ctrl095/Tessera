"""Check the handbook pages: each starts with a "# " title and has no trailing spaces."""

import pathlib
import sys


def main(src):
    problems = 0
    pages = sorted(pathlib.Path(src).glob("*.md"))
    for page in pages:
        lines = page.read_text().splitlines()
        if not lines or not lines[0].startswith("# "):
            print(f"{page}: the first line must be a '# ' title")
            problems += 1
        for n, line in enumerate(lines, 1):
            if line != line.rstrip():
                print(f"{page}:{n}: trailing whitespace")
                problems += 1
    print(f"lint: {len(pages)} pages, {problems} problems")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
