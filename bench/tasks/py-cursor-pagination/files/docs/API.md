# Catalogue API

## GET /books

Query parameters:

- `author` (optional): only this author's books.

Response: `{"books": [{"id", "title", "author", "year"}, ...]}`, ordered by title, then id.

## GET /books/<id>

One book, or 404 `{"error": "no such book"}`.
