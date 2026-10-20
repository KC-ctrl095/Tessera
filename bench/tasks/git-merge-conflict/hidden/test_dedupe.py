import unittest

import config
import sync
from test_sync import FakeServer, tickets


class DuplicateTicketsTest(unittest.TestCase):
    def test_a_ticket_that_moved_pages_is_kept_once(self):
        size = config.PAGE_SIZE
        first = tickets(1, size)
        # Ticket 7 was updated while we paged and moved to the end of the list.
        second = tickets(size + 1, 3) + [{"id": 7, "subject": "ticket 7 (edited)"}]
        server = FakeServer([first, second])
        result = sync.fetch_all(server)
        self.assertEqual(len(result), size + 3)
        self.assertEqual([t for t in result if t["id"] == 7], [{"id": 7, "subject": "ticket 7 (edited)"}])


if __name__ == "__main__":
    unittest.main()
