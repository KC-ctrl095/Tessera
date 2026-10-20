"""An in-process event bus. Every event is also written to the audit log."""

import json
import logging
from collections import defaultdict

log = logging.getLogger(__name__)
audit = logging.getLogger("dockyard.audit")


class EventBus:
    def __init__(self):
        self._subscribers = defaultdict(list)
        self.history = []

    def subscribe(self, name, fn):
        self._subscribers[name].append(fn)
        log.debug("subscribed %s to %s", getattr(fn, "__name__", fn), name)

    def publish(self, name, **payload):
        self.history.append((name, payload))
        audit.debug("event %s %s", name, json.dumps(payload, sort_keys=True, default=str))
        for fn in list(self._subscribers[name]) + list(self._subscribers["*"]):
            try:
                fn(name, payload)
            except Exception:
                # A broken subscriber must not stop the order flow.
                log.exception("subscriber %s failed on %s", getattr(fn, "__name__", fn), name)

    def count(self, name):
        return sum(1 for n, _ in self.history if n == name)
