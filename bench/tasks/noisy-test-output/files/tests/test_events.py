import unittest

from dockyard.events import EventBus
from dockyard.fixtures import make_context, make_orders
from dockyard.pipeline import Fulfilment


class EventBusTest(unittest.TestCase):
    def test_subscriber_gets_the_payload(self):
        bus, seen = EventBus(), []
        bus.subscribe("order.planned", lambda n, p: seen.append((n, p["order"])))
        bus.publish("order.planned", order="A-1")
        self.assertEqual(seen, [("order.planned", "A-1")])

    def test_wildcard_sees_everything(self):
        bus, seen = EventBus(), []
        bus.subscribe("*", lambda n, p: seen.append(n))
        bus.publish("a")
        bus.publish("b")
        self.assertEqual(seen, ["a", "b"])

    def test_broken_subscriber_does_not_stop_the_flow(self):
        bus, seen = EventBus(), []

        def broken(n, p):
            raise RuntimeError("subscriber bug")

        bus.subscribe("x", broken)
        bus.subscribe("x", lambda n, p: seen.append(n))
        with self.assertLogs("dockyard.events", "ERROR"):
            bus.publish("x")
        self.assertEqual(seen, ["x"])

    def test_history_and_count(self):
        bus = EventBus()
        for i in range(3):
            bus.publish("tick", i=i)
        bus.publish("tock")
        self.assertEqual((bus.count("tick"), bus.count("tock"), len(bus.history)), (3, 1, 4))

    def test_audit_log_has_the_payload_as_json(self):
        bus = EventBus()
        with self.assertLogs("dockyard.audit", "DEBUG") as cm:
            bus.publish("order.invoiced", order="B-2", total=12)
        self.assertIn('{"order": "B-2", "total": 12}', cm.output[0])

    def test_subscribers_run_in_order(self):
        bus, seen = EventBus(), []
        for i in range(4):
            bus.subscribe("e", lambda n, p, i=i: seen.append(i))
        bus.publish("e")
        self.assertEqual(seen, [0, 1, 2, 3])


class OrderEventsTest(unittest.TestCase):
    def test_a_day_publishes_one_event_per_step(self):
        ctx = make_context(9, stock_scale=4)
        statuses = []
        ctx.bus.subscribe("shipment.status", lambda n, p: statuses.append(p["status"]))
        s = Fulfilment(ctx).simulate_day(make_orders(ctx.catalog, 200, seed=9))
        self.assertEqual(ctx.bus.count("order.planned"), s["plans"])
        self.assertEqual(ctx.bus.count("order.invoiced"), s["orders"])
        self.assertEqual(len(statuses), s["plans"])
        self.assertEqual(statuses.count("FAILED_ATTEMPT"), s["failed_attempts"])


if __name__ == "__main__":
    unittest.main()
