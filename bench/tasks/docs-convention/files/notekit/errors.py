"""How commands report errors (see CONTRIBUTING.md)."""

import sys

EXIT_ERROR = 2


def fail(message):
    """Print `notekit: error: <message>` to stderr and return the exit code to use."""
    print(f"notekit: error: {message}", file=sys.stderr)
    return EXIT_ERROR
