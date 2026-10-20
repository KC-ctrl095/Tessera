from checklib import Check, run, tail, with_hidden

c = Check()
with with_hidden() as d:
    code, out = run("python3 -m unittest -v test_cache test_cache_spec", cwd=d, timeout=60)
c.ok(code == 0, "the cache follows its docstring (expiry boundary, falsy values, LRU, purge, len)" if code == 0 else "cache tests fail:\n" + tail(out, 3000))
c.done()
