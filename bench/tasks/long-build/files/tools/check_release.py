"""Check release.json against the changelog. Reports the first problem found."""

import json
import re
import sys


def fail(msg):
    print(f"release check failed: {msg}", file=sys.stderr)
    sys.exit(1)


release = json.load(open(sys.argv[1]))
changelog = open(sys.argv[2]).read()
entries = re.findall(r"^## (\d+\.\d+\.\d+) \((\d{4}-\d{2}-\d{2})\)", changelog, re.M)
if not entries:
    fail("the changelog has no release entries")
newest, newest_date = entries[0]
for key in ("name", "version", "released", "channel"):
    if key not in release:
        fail(f"release.json has no '{key}'")
if release["version"] != newest:
    fail(f"release.json says version {release['version']}, but the newest changelog entry is {newest}")
if release["released"] != newest_date:
    fail(f"release.json says it was released on {release['released']}, but the changelog dates {newest} {newest_date}")
if release["channel"] not in ("stable", "beta"):
    fail(f"unknown channel {release['channel']!r}")
print(f"    release.json matches the changelog ({newest}, {newest_date}, {release['channel']})")
