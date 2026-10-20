"""HTTP plumbing: requests in, JSON responses out."""

import json
import sys
import traceback
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler
from urllib.parse import urlsplit

from . import __version__, handlers, query
from .errors import ApiError, bad_request

MAX_BODY = 64 * 1024


@dataclass
class Context:
    config: object
    index: object
    base_url: str = ""


class Response:
    def __init__(self, status, payload, headers):
        self.status = status
        self.body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        # HTTP/1.1 header values are ISO-8859-1 at most; anything else must be
        # percent-encoded by whoever built the value.
        self.headers = [(name, str(value).encode("latin-1").decode("latin-1")) for name, value in headers.items()]


def make_handler(config, index):
    class Handler(BaseHTTPRequestHandler):
        server_version = f"parcelhub/{__version__}"

        def do_GET(self):
            self._dispatch("GET")

        def do_POST(self):
            self._dispatch("POST")

        def _body(self):
            length = int(self.headers.get("Content-Length") or 0)
            if not length:
                return None
            if length > MAX_BODY:
                raise bad_request("request body too large")
            try:
                return json.loads(self.rfile.read(length).decode("utf-8"))
            except (UnicodeDecodeError, ValueError):
                raise bad_request("the body is not valid JSON")

        def _dispatch(self, method):
            url = urlsplit(self.path)
            ctx = Context(config, index, f"http://{self.headers.get('Host') or f'{config.host}:{config.port}'}")
            try:
                params = query.parse(url.query)
                status, payload, headers = handlers.route(ctx, method, url.path, params, self._body())
                response = Response(status, payload, headers)
            except ApiError as e:
                response = Response(e.status, {"error": e.message}, {})
            except Exception:
                print(f"error handling {method} {self.path}:", file=sys.stderr)
                traceback.print_exc()
                response = Response(500, {"error": "internal server error"}, {})
            self.send_response(response.status)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(response.body)))
            for name, value in response.headers:
                self.send_header(name, value)
            self.end_headers()
            self.wfile.write(response.body)

    return Handler
