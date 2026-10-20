"""Run the API: python3 -m parcelhub.serve

Reads config/dev.json (or the file named by PARCELHUB_CONFIG), loads and
indexes the parcels (about 15 seconds), prints "ready" and then serves until it
is stopped with Ctrl+C or SIGTERM. While it runs, its process id is in
run/parcelhub.pid.
"""

import os
import signal
import sys
import threading
from http.server import ThreadingHTTPServer
from pathlib import Path

from . import __version__, config, index, store
from .app import make_handler


def main():
    cfg = config.load()
    print(f"parcelhub {__version__} starting (config {cfg.path})", flush=True)
    parcels = store.load(cfg.data_path)
    print(f"loaded {len(parcels)} parcels from {cfg.data_path.name}", flush=True)
    idx = index.build(parcels, progress=lambda msg: print(msg, flush=True))
    server = ThreadingHTTPServer((cfg.host, cfg.port), make_handler(cfg, idx))
    server.daemon_threads = True
    pidfile = cfg.path.parent.parent / "run" / "parcelhub.pid"
    pidfile.parent.mkdir(exist_ok=True)
    pidfile.write_text(f"{os.getpid()}\n")

    def stop(signum, frame):
        print(f"parcelhub: got signal {signum}, shutting down", flush=True)
        threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    print(f"parcelhub ready on http://{cfg.host}:{cfg.port}", flush=True)
    try:
        server.serve_forever()
    finally:
        server.server_close()
        pidfile.unlink(missing_ok=True)
        print("parcelhub stopped", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
