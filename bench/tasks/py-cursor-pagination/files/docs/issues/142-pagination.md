# 142: Paginate GET /books

The books table has about two million rows in production, and GET /books
returns all of them. Add cursor-based paging:

- New query parameter `limit`: how many books per page, an integer from 1 to
  100, default 20. Anything else is a 400 with
  `{"error": "limit must be an integer from 1 to 100"}`.
- New query parameter `after`: the `next_cursor` of the previous page. A
  cursor we can't read is a 400 with `{"error": "invalid cursor"}`.
- The response gets `next_cursor`: a cursor for the page after this one, or
  `null` when there are no more books (also when the last page is exactly
  full).
- Order stays title, then id. Use the helpers in app/cursor.py: a cursor
  holds the (title, id) of the last book on the page, so paging stays correct
  when books are added or removed between requests (no offsets).
- The database must do the work: filter and limit in the SQL query, never
  load the whole table.
- `author` keeps working together with paging.
- Update docs/API.md.
