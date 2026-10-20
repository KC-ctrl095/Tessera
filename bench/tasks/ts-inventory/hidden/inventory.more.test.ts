import { test } from "node:test";
import assert from "node:assert/strict";

import { applyOrders, reserve } from "./inventory.ts";

test("reserve leaves the caller's stock object alone when the order goes through", () => {
  const stock = { "MUG-01": 5, "TEA-12": 3 };
  const result = reserve(stock, { id: "x1", lines: [{ sku: "MUG-01", qty: 5 }] });
  assert.equal(result.ok, true);
  assert.deepEqual(stock, { "MUG-01": 5, "TEA-12": 3 });
  assert.deepEqual(result.stock, { "MUG-01": 0, "TEA-12": 3 });
});

test("reserve leaves the caller's stock object alone when the order is refused", () => {
  const stock = { "MUG-01": 5, "TEA-12": 3 };
  const result = reserve(stock, { id: "x2", lines: [{ sku: "MUG-01", qty: 1 }, { sku: "TEA-12", qty: 1 }, { sku: "PAN-03", qty: 1 }] });
  assert.equal(result.ok, false);
  assert.deepEqual(stock, { "MUG-01": 5, "TEA-12": 3 });
  assert.deepEqual(result.stock, { "MUG-01": 5, "TEA-12": 3 });
});

test("lines for the same SKU add up", () => {
  const refused = reserve({ "MUG-01": 3 }, { id: "x3", lines: [{ sku: "MUG-01", qty: 2 }, { sku: "MUG-01", qty: 2 }] });
  assert.equal(refused.ok, false);
  assert.deepEqual(refused.stock, { "MUG-01": 3 });
  const fits = reserve({ "MUG-01": 4 }, { id: "x4", lines: [{ sku: "MUG-01", qty: 2 }, { sku: "MUG-01", qty: 2 }] });
  assert.equal(fits.ok, true);
  assert.deepEqual(fits.stock, { "MUG-01": 0 });
});

test("a refused order holds nothing back for later orders in a batch", () => {
  const result = applyOrders({ "MUG-01": 3, "TEA-12": 2 }, [
    { id: "a", lines: [{ sku: "MUG-01", qty: 3 }, { sku: "TEA-12", qty: 5 }] },
    { id: "b", lines: [{ sku: "MUG-01", qty: 3 }] },
    { id: "c", lines: [{ sku: "TEA-12", qty: 2 }] },
  ]);
  assert.deepEqual(result.accepted, ["b", "c"]);
  assert.deepEqual(result.rejected.map((r) => r.id), ["a"]);
  assert.deepEqual(result.stock, { "MUG-01": 0, "TEA-12": 0 });
});

test("applyOrders leaves the caller's stock object alone", () => {
  const stock = { "MUG-01": 4 };
  const result = applyOrders(stock, [
    { id: "a", lines: [{ sku: "MUG-01", qty: 1 }] },
    { id: "b", lines: [{ sku: "MUG-01", qty: 3 }] },
    { id: "c", lines: [{ sku: "MUG-01", qty: 1 }] },
  ]);
  assert.deepEqual(stock, { "MUG-01": 4 });
  assert.deepEqual(result.accepted, ["a", "b"]);
  assert.deepEqual(result.stock, { "MUG-01": 0 });
});

test("an unknown SKU after good lines refuses the whole order", () => {
  const result = reserve({ "MUG-01": 2, "TEA-12": 2 }, { id: "x5", lines: [{ sku: "MUG-01", qty: 2 }, { sku: "TEA-12", qty: 2 }, { sku: "CUP-99", qty: 1 }] });
  assert.equal(result.ok, false);
  assert.match(result.error ?? "", /CUP-99/);
  assert.deepEqual(result.stock, { "MUG-01": 2, "TEA-12": 2 });
});

test("quantities must be whole numbers above zero", () => {
  for (const qty of [0, -1, 1.5, Number.NaN]) {
    const result = reserve({ "MUG-01": 5 }, { id: "x6", lines: [{ sku: "MUG-01", qty: 1 }, { sku: "MUG-01", qty }] });
    assert.equal(result.ok, false, `qty ${qty}`);
    assert.deepEqual(result.stock, { "MUG-01": 5 });
  }
});

test("an empty batch changes nothing", () => {
  const result = applyOrders({ "MUG-01": 1 }, []);
  assert.deepEqual(result, { stock: { "MUG-01": 1 }, accepted: [], rejected: [] });
});
