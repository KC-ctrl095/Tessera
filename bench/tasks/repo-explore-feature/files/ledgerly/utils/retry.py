"""Retries with exponential backoff for flaky integrations."""

import time


def retry(fn, attempts=3, base=0.5, sleep=time.sleep):
    for i in range(attempts):
        try:
            return fn()
        except OSError:
            if i == attempts - 1:
                raise
            sleep(base * 2 ** i)
