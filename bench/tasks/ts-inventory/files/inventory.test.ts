import { test } from "node:test";
import assert from "node:assert/strict";

import { applyOrders, lowStock, reserve } from "./inventory.ts";

test("reserves every line of an order", () => {
  const result = reserve({ "MUG-01": 5, "TEA-12": 3 }, { id: "o1", lines: [{ sku: "MUG-01", qty: 2 }, { sku: "TEA-12", qty: 1 }] });
  assert.equal(result.ok, true);
  assert.deepEqual(result.stock, { "MUG-01": 3, "TEA-12": 2 });
});

test("an order can take the last units of a SKU", () => {
  const result = reserve({ "MUG-01": 2 }, { id: "o2", lines: [{ sku: "MUG-01", qty: 2 }] });
  assert.equal(result.ok, true);
  assert.deepEqual(result.stock, { "MUG-01": 0 });
});

test("a refused order leaves the stock as it was", () => {
  const result = reserve(
    { "MUG-01": 5, "TEA-12": 1 },
    { id: "o3", lines: [{ sku: "MUG-01", qty: 2 }, { sku: "TEA-12", qty: 4 }] },
  );
  assert.equal(result.ok, false);
  assert.match(result.error ?? "", /TEA-12/);
  assert.deepEqual(result.stock, { "MUG-01": 5, "TEA-12": 1 });
});

test("unknown SKUs and bad quantities are refused", () => {
  assert.equal(reserve({ "MUG-01": 5 }, { id: "o4", lines: [{ sku: "CUP-99", qty: 1 }] }).ok, false);
  assert.equal(reserve({ "MUG-01": 5 }, { id: "o5", lines: [{ sku: "MUG-01", qty: 0 }] }).ok, false);
});

test("applyOrders goes through the orders in turn", () => {
  const result = applyOrders({ "MUG-01": 4, "TEA-12": 10 }, [
    { id: "a", lines: [{ sku: "MUG-01", qty: 3 }] },
    { id: "b", lines: [{ sku: "MUG-01", qty: 2 }] },
    { id: "c", lines: [{ sku: "TEA-12", qty: 10 }] },
  ]);
  assert.deepEqual(result.accepted, ["a", "c"]);
  assert.deepEqual(result.rejected.map((r) => r.id), ["b"]);
  assert.deepEqual(result.stock, { "MUG-01": 1, "TEA-12": 0 });
});

test("lowStock lists SKUs at or below their reorder point", () => {
  assert.deepEqual(lowStock({ "MUG-01": 2, "TEA-12": 9, "PAN-03": 4 }, { "TEA-12": 10 }), ["MUG-01", "PAN-03", "TEA-12"]);
});
