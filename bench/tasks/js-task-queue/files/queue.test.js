import { test } from "node:test";
import assert from "node:assert/strict";

import { TaskQueue } from "./queue.js";

/** A promise with its resolve and reject exposed, so a test decides when a job ends. */
function deferred() {
  let resolve, reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** Lets every pending callback and promise reaction run. */
async function flush() {
  for (let i = 0; i < 3; i++) {
    await new Promise((r) => setImmediate(r));
  }
}

/** Jobs that wait for the test to finish them, and record how many run at once. */
function tracker() {
  const t = { active: 0, max: 0, started: [], gates: [] };
  t.job = (name) => () => {
    t.active++;
    t.max = Math.max(t.max, t.active);
    t.started.push(name);
    const gate = deferred();
    t.gates.push(gate);
    return gate.promise.finally(() => t.active--);
  };
  return t;
}

test("never runs more than `concurrency` jobs at once", async () => {
  const q = new TaskQueue({ concurrency: 2 });
  const t = tracker();
  const results = ["a", "b", "c", "d", "e"].map((name) => q.add(t.job(name)));
  await flush();
  assert.deepEqual(t.started, ["a", "b"]);
  t.gates[0].resolve("A");
  await flush();
  assert.deepEqual(t.started, ["a", "b", "c"]);
  for (let i = 1; i < 5; i++) {
    t.gates[i].resolve(String.fromCharCode(65 + i));
    await flush();
  }
  assert.deepEqual(await Promise.all(results), ["A", "B", "C", "D", "E"]);
  assert.equal(t.max, 2);
});

test("a job that throws rejects its promise and the queue keeps going", async () => {
  const q = new TaskQueue({ concurrency: 1 });
  const failing = q.add(async () => {
    throw new Error("disk full");
  });
  const next = q.add(async () => "next one ran");
  await assert.rejects(failing, /disk full/);
  assert.equal(await next, "next one ran");
});

test("onIdle waits for running jobs", async () => {
  const q = new TaskQueue({ concurrency: 2 });
  const t = tracker();
  q.add(t.job("a"));
  q.add(t.job("b"));
  let idle = false;
  q.onIdle().then(() => (idle = true));
  await flush();
  assert.equal(idle, false);
  t.gates[0].resolve();
  await flush();
  assert.equal(idle, false);
  t.gates[1].resolve();
  await flush();
  assert.equal(idle, true);
});
