"""A pass-through between Claude Code and llama.cpp's Anthropic-compatible API.

    python3 msgshim.py UPSTREAM MODEL PORT_FILE

Claude Code (2.1.x) sends its system prompt as several text blocks, the first
one a billing header that Anthropic's API strips, and puts a `system`-role
message (its environment section) after the user's first message. Qwen's chat
template accepts one system message, at the start, and fails the request
otherwise ("System message must be at the beginning"). This adapts only the
shape of /v1/messages requests:

- the system blocks are joined into one string (without the billing header);
- a `system`-role message inside `messages` becomes user-turn text at the same
  place (merged into the user message before it), so the prompt keeps its
  order and llama.cpp's prompt cache keeps working;
- the model is pinned to MODEL (the server may serve only that one).

Everything else, including streamed responses, passes through unchanged.
It listens on 127.0.0.1 on a free port, written to PORT_FILE.
"""

import http.server
import json
import os
import sys
import urllib.error
import urllib.request

UPSTREAM, MODEL, PORT_FILE = sys.argv[1].rstrip("/"), sys.argv[2], sys.argv[3]
HOP = {"host", "content-length", "connection", "accept-encoding", "transfer-encoding", "keep-alive"}


def adapt(data):
    system = data.get("system")
    if isinstance(system, list):
        texts = [b.get("text", "") for b in system if isinstance(b, dict) and b.get("type") == "text"]
        texts = [t for t in texts if not t.startswith("x-anthropic-billing-header:")]
        data["system"] = "\n\n".join(t.strip("\n") for t in texts if t.strip())
    messages = []
    for m in data.get("messages") or []:
        if m.get("role") == "system":
            content = m.get("content")
            blocks = [{"type": "text", "text": content}] if isinstance(content, str) else list(content or [])
            if messages and messages[-1].get("role") == "user":
                prev = messages[-1]
                if isinstance(prev.get("content"), str):
                    prev["content"] = [{"type": "text", "text": prev["content"]}]
                prev["content"] = list(prev["content"]) + blocks
            else:
                messages.append({"role": "user", "content": blocks})
        else:
            messages.append(m)
    if "messages" in data:
        data["messages"] = messages
    if "model" in data:
        data["model"] = MODEL
    return data


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _forward(self, method):
        n = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(n) if n else None
        if body and self.path.split("?")[0].startswith("/v1/messages"):
            try:
                body = json.dumps(adapt(json.loads(body))).encode()
            except ValueError:
                pass
        req = urllib.request.Request(UPSTREAM + self.path, data=body, method=method)
        for k, v in self.headers.items():
            if k.lower() not in HOP:
                req.add_header(k, v)
        try:
            resp = urllib.request.urlopen(req, timeout=3600)
            status = resp.status
        except urllib.error.HTTPError as e:
            resp, status = e, e.code
        except OSError as e:
            msg = json.dumps({"type": "error", "error": {"type": "api_error", "message": f"upstream: {e}"}}).encode()
            self.send_response(502)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(msg)))
            self.end_headers()
            self.wfile.write(msg)
            return
        self.send_response(status)
        for k, v in resp.headers.items():
            if k.lower() not in HOP:
                self.send_header(k, v)
        self.send_header("transfer-encoding", "chunked")
        self.end_headers()
        try:
            while True:
                chunk = resp.read1(65536) if hasattr(resp, "read1") else resp.read(65536)
                if not chunk:
                    break
                self.wfile.write(b"%x\r\n%s\r\n" % (len(chunk), chunk))
                self.wfile.flush()
            self.wfile.write(b"0\r\n\r\n")
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            resp.close()

    def do_POST(self):
        self._forward("POST")

    def do_GET(self):
        self._forward("GET")


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
server.daemon_threads = True
tmp = PORT_FILE + ".tmp"
with open(tmp, "w") as f:
    f.write(str(server.server_address[1]))
os.replace(tmp, PORT_FILE)
server.serve_forever()
