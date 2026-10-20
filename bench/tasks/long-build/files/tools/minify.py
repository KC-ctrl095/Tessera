"""Strip indentation and blank lines from an HTML template."""

import sys

with open(sys.argv[1]) as f:
    print("\n".join(line.strip() for line in f if line.strip()))
