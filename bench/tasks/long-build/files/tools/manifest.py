"""Print a MANIFEST for the build: version, then a hash of every file."""

import hashlib
import os
import sys

root, version = sys.argv[1], sys.argv[2]
lines = []
for dirpath, dirnames, filenames in os.walk(root):
    dirnames.sort()
    for name in sorted(filenames):
        if name == "MANIFEST" or name.endswith(".pyc"):
            continue
        path = os.path.join(dirpath, name)
        digest = hashlib.sha256(open(path, "rb").read()).hexdigest()[:16]
        lines.append(f"{os.path.relpath(path, root)} {digest}")
stamp = hashlib.sha256("\n".join(lines).encode()).hexdigest()[:16]
print(f"version: {version}")
print(f"build: {stamp}")
print("\n".join(lines))
