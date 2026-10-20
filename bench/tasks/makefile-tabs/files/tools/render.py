"""Render the handbook pages (Markdown-ish) into one HTML file."""

import html
import pathlib
import sys


def render(src, out):
    pages = sorted(pathlib.Path(src).glob("*.md"))
    parts = ["<!doctype html><title>Handbook</title>"]
    for page in pages:
        for line in page.read_text().splitlines():
            if line.startswith("# "):
                parts.append(f"<h1>{html.escape(line[2:])}</h1>")
            elif line.strip():
                parts.append(f"<p>{html.escape(line)}</p>")
    target = pathlib.Path(out) / "index.html"
    target.write_text("\n".join(parts) + "\n")
    print(f"wrote {target} ({len(pages)} pages)")


if __name__ == "__main__":
    render(sys.argv[1], sys.argv[2])
