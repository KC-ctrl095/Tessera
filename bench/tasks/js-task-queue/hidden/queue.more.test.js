import { test } from "node:test";
import assert from "node:assert/strict";

import { TaskQueue } from "./queue.js";

function deferred() {
  let resolve, reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

async function flush() {
  await new Promise((r) => setTimeout(r, 0));
  for (let i = 0; i < 4; i++) {
    await new Promise((r) => setImmediate(r));
  }
}

function tracker() {
  const t = { active: 0, max: 0, started: [], gates: {} };
  t.job = (name) => () => {
    t.active++;
    t.max = Math.max(t.max, t.active);
    t.started.push(name);
    const gate = deferred();
    t.gates[name] = gate;
    return gate.promise.finally(() => t.active--);
  };
  return t;
}

test("concurrency 1 runs jobs one by one, in the order they were added", async () => {
  const q = new TaskQueue({ concurrency: 1 });
  const t = tracker();
  const names = ["a", "b", "c", "d"];
  const results = names.map((n) => q.add(t.job(n)));
  for (const n of names) {
    await flush();
    assert.deepEqual(t.started, names.slice(0, names.indexOf(n) + 1));
    t.gates[n].resolve(n.toUpperCase());
  }
  assert.deepEqual(await Promise.all(results), ["A", "B", "C", "D"]);
  assert.equal(t.max, 1);
});

test("concurrency 3 fills each free slot with the next waiting job", async () => {
  const q = new TaskQueue({ concurrency: 3 });
  const t = tracker();
  const names = ["a", "b", "c", "d", "e", "f", "g"];
  const results = names.map((n) => q.add(t.job(n)));
  await flush();
  assert.deepEqual(t.started, ["a", "b", "c"]);
  t.gates.b.resolve(2);
  await flush();
  assert.deepEqual(t.started, ["a", "b", "c", "d"]);
  t.gates.d.resolve(4);
  t.gates.a.resolve(1);
  await flush();
  assert.deepEqual(t.started, ["a", "b", "c", "d", "e", "f"]);
  assert.equal(t.active, 3);
  for (const n of ["c", "e", "f", "g"]) {
    t.gates[n].resolve(names.indexOf(n) + 1);
    await flush();
  }
  assert.deepEqual(await Promise.all(results), [1, 2, 3, 4, 5, 6, 7]);
  assert.equal(t.max, 3);
});

test("promises settle in the order the jobs finish", async () => {
  const q = new TaskQueue({ concurrency: 3 });
  const t = tracker();
  const settled = [];
  for (const n of ["a", "b", "c"]) {
    q.add(t.job(n)).then((v) => settled.push(v), (e) => settled.push("error " + e.message));
  }
  await flush();
  t.gates.c.resolve("c done");
  await flush();
  t.gates.a.reject(new Error("a failed"));
  await flush();
  t.gates.b.resolve("b done");
  await flush();
  assert.deepEqual(settled, ["c done", "error a failed", "b done"]);
});

test("a job that throws synchronously rejects its promise and frees its slot", async () => {
  const q = new TaskQueue({ concurrency: 1 });
  const boom = q.add(() => {
    throw new TypeError("not a picture");
  });
  const after = q.add(() => 42);
  await assert.rejects(boom, TypeError);
  assert.equal(await after, 42);
});

test("failed jobs don't leave extra slots behind", async () => {
  const q = new TaskQueue({ concurrency: 1 });
  for (let i = 0; i < 3; i++) {
    await assert.rejects(q.add(async () => {
      throw new Error("bad input " + i);
    }));
  }
  await assert.rejects(q.add(() => {
    throw new Error("bad input 3");
  }));
  const t = tracker();
  const results = ["a", "b", "c"].map((n) => q.add(t.job(n)));
  await flush();
  assert.deepEqual(t.started, ["a"]);
  for (const n of ["a", "b", "c"]) {
    t.gates[n].resolve(n);
    await flush();
  }
  assert.deepEqual(await Promise.all(results), ["a", "b", "c"]);
  assert.equal(t.max, 1);
});

test("jobs may return plain values", async () => {
  const q = new TaskQueue({ concurrency: 2 });
  assert.deepEqual(await Promise.all([q.add(() => 1), q.add(() => "two"), q.add(() => null)]), [1, "two", null]);
});

test("onIdle resolves at once when the queue is empty", async () => {
  const q = new TaskQueue({ concurrency: 2 });
  let idle = false;
  q.onIdle().then(() => (idle = true));
  await flush();
  assert.equal(idle, true);
});

test("onIdle waits for queued jobs, not only the running ones", async () => {
  const q = new TaskQueue({ concurrency: 1 });
  const t = tracker();
  q.add(t.job("a"));
  q.add(t.job("b"));
  q.add(t.job("c"));
  let idle = 0;
  q.onIdle().then(() => idle++);
  await flush();
  q.onIdle().then(() => idle++);
  for (const n of ["a", "b"]) {
    t.gates[n].resolve();
    await flush();
    assert.equal(idle, 0, `still busy after ${n}`);
  }
  t.gates.c.resolve();
  await flush();
  assert.equal(idle, 2);
});

test("onIdle resolves after a failed last job", async () => {
  const q = new TaskQueue({ concurrency: 2 });
  const t = tracker();
  const p = q.add(t.job("a"));
  p.catch(() => {});
  let idle = false;
  q.onIdle().then(() => (idle = true));
  await flush();
  assert.equal(idle, false);
  t.gates.a.reject(new Error("upload failed"));
  await flush();
  assert.equal(idle, true);
});

test("a job can add more jobs", async () => {
  const q = new TaskQueue({ concurrency: 1 });
  const order = [];
  const inner = [];
  await q.add(async () => {
    order.push("outer");
    inner.push(q.add(async () => order.push("inner")));
  });
  await Promise.all(inner);
  await q.onIdle();
  assert.deepEqual(order, ["outer", "inner"]);
});
