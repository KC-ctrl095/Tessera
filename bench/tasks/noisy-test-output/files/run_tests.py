#!/usr/bin/env python3
"""Run dockyard's test suite the way CI does: every log line at DEBUG, warnings on.

    python3 run_tests.py                   # every test
    python3 run_tests.py routing packing   # only tests whose id contains one of the words
"""

import logging
import sys
import time
import traceback
import unittest
import warnings
from pathlib import Path

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

RULE = "-" * 70


def configure_logging():
    logging.basicConfig(
        level=logging.DEBUG,
        stream=sys.stdout,
        format="%(asctime)s.%(msecs)03d %(levelname)-7s %(name)s: %(message)s",
        datefmt="%H:%M:%S",
    )
    logging.captureWarnings(True)
    warnings.simplefilter("always")


def flatten(suite):
    for t in suite:
        if isinstance(t, unittest.TestSuite):
            yield from flatten(t)
        else:
            yield t


def collect(words):
    suite = unittest.TestLoader().discover(str(ROOT / "tests"), top_level_dir=str(ROOT))
    tests = sorted(flatten(suite), key=lambda t: t.id())
    if words:
        tests = [t for t in tests if any(w in t.id() for w in words)]
    return tests


class InlineResult(unittest.TestResult):
    """Prints each result as it happens, failures in full right where they occur."""

    def __init__(self, total):
        super().__init__()
        self.total = total
        self.n = 0
        self.failed = 0

    def startTest(self, test):
        super().startTest(test)
        self.n += 1
        self._t0 = time.time()

    def _status(self, test, word):
        print(f"[{self.n:>4}/{self.total}] {test.id()} ... {word} ({time.time() - self._t0:.3f}s)", flush=True)

    def addSuccess(self, test):
        super().addSuccess(test)
        self._status(test, "ok")

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        self._status(test, f"skipped ({reason})")

    def _report(self, test, kind, err):
        self.failed += 1
        self._status(test, kind)
        print("=" * 70)
        print(f"{kind}: {test.id()}")
        print(RULE)
        print("".join(traceback.format_exception(*err)).rstrip())
        print("=" * 70, flush=True)

    def addFailure(self, test, err):
        super().addFailure(test, err)
        self._report(test, "FAIL", err)

    def addError(self, test, err):
        super().addError(test, err)
        self._report(test, "ERROR", err)


def main(words):
    configure_logging()
    tests = collect(words)
    result = InlineResult(len(tests))
    t0 = time.time()
    for test in tests:
        test(result)
    from dockyard import store

    store.close_all()
    print(RULE)
    print(f"Ran {result.n} tests in {time.time() - t0:.3f}s")
    print()
    if result.failed:
        print(f"FAILED ({result.failed} failure{'s' if result.failed != 1 else ''})")
        return 1
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
