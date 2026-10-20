// A small promise queue: runs at most `concurrency` jobs at a time, first in
// first out. Used by the thumbnail worker and the import scripts.

export class TaskQueue {
  /** @param {{ concurrency?: number }} [options] */
  constructor({ concurrency = 1 } = {}) {
    if (!Number.isInteger(concurrency) || concurrency < 1) {
      throw new RangeError("concurrency must be a whole number of at least 1");
    }
    this.concurrency = concurrency;
    this.running = 0;
    this.waiting = [];
    this.idleWaiters = [];
  }

  /**
   * Queues a job. `fn` is called with no arguments and may return a value or a
   * promise; the returned promise settles the same way the job does.
   */
  add(fn) {
    return new Promise((resolve, reject) => {
      this.waiting.push({ fn, resolve, reject });
      this._next();
    });
  }

  /** Resolves once no job is waiting or running. */
  onIdle() {
    if (this.waiting.length === 0) {
      return Promise.resolve();
    }
    return new Promise((resolve) => this.idleWaiters.push(resolve));
  }

  /** Jobs waiting for a free slot. */
  get size() {
    return this.waiting.length;
  }

  /** Jobs running right now. */
  get pending() {
    return this.running;
  }

  _next() {
    if (this.running >= this.concurrency) {
      return;
    }
    const job = this.waiting.shift();
    if (!job) {
      this._wakeIdle();
      return;
    }
    // Start on a fresh microtask so a job never runs inside the caller's add().
    queueMicrotask(async () => {
      this.running++;
      const value = await job.fn();
      job.resolve(value);
      this.running--;
      this._next();
    });
  }

  _wakeIdle() {
    const waiters = this.idleWaiters;
    this.idleWaiters = [];
    for (const resolve of waiters) {
      resolve();
    }
  }
}
