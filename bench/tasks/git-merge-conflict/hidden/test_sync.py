import io
import json
import unittest

import config
import sync


class FakeServer:
    """Stands in for urlopen: serves pages from a list and records requests."""

    def __init__(self, pages):
        self.pages = pages
        self.requests = []

    def __call__(self, req, timeout):
        self.requests.append((req.full_url, timeout))
        page = int(req.full_url.split("page=")[1].split("&")[0])
        body = {"tickets": self.pages[page - 1] if page <= len(self.pages) else []}
        return io.BytesIO(json.dumps(body).encode())


def tickets(start, n):
    return [{"id": i, "subject": f"ticket {i}"} for i in range(start, start + n)]


class SyncTest(unittest.TestCase):
    def test_single_short_page(self):
        server = FakeServer([tickets(1, 3)])
        self.assertEqual([t["id"] for t in sync.fetch_all(server)], [1, 2, 3])
        self.assertEqual(len(server.requests), 1)

    def test_pages_until_short(self):
        size = config.PAGE_SIZE
        server = FakeServer([tickets(1, size), tickets(size + 1, size), tickets(2 * size + 1, 2)])
        self.assertEqual(len(sync.fetch_all(server)), 2 * size + 2)
        self.assertEqual(len(server.requests), 3)

    def test_timeout_is_passed(self):
        server = FakeServer([[]])
        sync.fetch_all(server)
        self.assertEqual(server.requests[0][1], config.TIMEOUT)

    def test_retries_a_failed_request(self):
        server = FakeServer([tickets(1, 2)])
        failures = [OSError("connection reset"), OSError("timed out")]

        def flaky(req, timeout):
            if failures:
                raise failures.pop(0)
            return server(req, timeout)

        waits = []
        self.assertEqual([t["id"] for t in sync.get_page(1, flaky, waits.append)], [1, 2])
        self.assertEqual(waits, [config.BACKOFF, config.BACKOFF * 2])

    def test_gives_up_after_the_last_retry(self):
        calls = []

        def down(req, timeout):
            calls.append(req.full_url)
            raise OSError("no route to host")

        waits = []
        with self.assertRaises(OSError):
            sync.get_page(1, down, waits.append)
        self.assertEqual(len(calls), config.RETRIES + 1)
        self.assertEqual(len(waits), config.RETRIES)


if __name__ == "__main__":
    unittest.main()
