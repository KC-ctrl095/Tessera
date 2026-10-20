set -e
cp "$BENCH_SOLUTION"/app/*.py app/
cat >> docs/API.md <<'MD'

### Paging

GET /books also takes `limit` (1 to 100, default 20) and `after` (the
`next_cursor` of the previous page). The response has `next_cursor`: the
cursor of the next page, or null on the last page.
MD
cat > tests/test_paging.py <<'PY'
import unittest

from app.db import connect
from app.handlers import get_books
from app.repository import BookRepository


class PagingTest(unittest.TestCase):
    def test_two_pages(self):
        repo = BookRepository(connect())
        status, first = get_books(repo, {"limit": "20"})
        status, second = get_books(repo, {"after": first["next_cursor"]})
        self.assertEqual(len(first["books"]) + len(second["books"]), 26)
        self.assertIsNone(second["next_cursor"])
PY
