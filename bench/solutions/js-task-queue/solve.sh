set -e
python3 - <<'PY'
p = "queue.js"
s = open(p).read()
s = s.replace("""    if (this.waiting.length === 0) {
      return Promise.resolve();""", """    if (this.waiting.length === 0 && this.running === 0) {
      return Promise.resolve();""")
old = s[s.index("  _next() {"):s.index("  _wakeIdle() {")]
new = """  _next() {
    if (this.running >= this.concurrency) {
      return;
    }
    const job = this.waiting.shift();
    if (!job) {
      if (this.running === 0) {
        this._wakeIdle();
      }
      return;
    }
    // Count the slot now: a microtask later, other add() calls would see it free.
    this.running++;
    // Start on a fresh microtask so a job never runs inside the caller's add().
    queueMicrotask(async () => {
      try {
        job.resolve(await job.fn());
      } catch (err) {
        job.reject(err);
      } finally {
        this.running--;
        this._next();
      }
    });
  }

"""
s = s.replace(old, new)
open(p, "w").write(s)
PY
