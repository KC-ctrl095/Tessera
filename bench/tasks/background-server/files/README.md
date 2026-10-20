# parcelhub

The parcel tracking API: JSON over HTTP, standard library only.

```
python3 -m parcelhub.serve        # dev server; loads and indexes for ~15 s, then prints "ready"
python3 tests/integration.py      # checks against the running dev server
```

The dev server reads `config/dev.json` (host, port, data file, page sizes; copy
`config/dev.json.example` if you don't have one). It runs until stopped with
Ctrl+C or SIGTERM, and keeps its process id in `run/parcelhub.pid` while it runs.

## API

- `GET /health`
- `GET /parcels?status=&city=&recipient=&q=&page=&per_page=`: a page of
  parcels in id order. `X-Total-Count` has the number of matches; `Link` has
  `prev`/`next` URLs (RFC 8288) when there is a previous or next page. City
  and recipient matching ignores case and accents.
- `GET /parcels/<id>`: one parcel with its tracking events.
- `POST /parcels/<id>/events` with `{"status": "...", "location": "..."}`:
  adds a tracking event (kept in memory only).

Errors are JSON: `{"error": "..."}` with a 4xx status.
