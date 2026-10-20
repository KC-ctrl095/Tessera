import copy

import minitoml
from checklib import Check, original_text, text

REL = "settings.toml"
c = Check()

orig_src = original_text(REL)
orig = minitoml.loads(orig_src)
expected = copy.deepcopy(orig)
server = expected["server"]
server["bind"] = server.pop("listen")
server["timeout_ms"] = server.pop("timeout") * 1000
log = expected["log"]
log["file"] = {"path": log["file"], "keep_days": 14}
expected = {"config_version": 3, **expected}

src = text(REL)
new = None
if src is None:
    c.fail(f"{REL} is missing")
else:
    try:
        new = minitoml.loads(src)
    except minitoml.TomlError as e:
        c.fail(f"{REL} isn't valid TOML any more: {e}")

if new is not None:
    c.ok(new.get("config_version") == 3, "config_version = 3 at the top level")
    s = new.get("server", {})
    c.ok(s.get("bind") == orig["server"]["listen"] and "listen" not in s, "[server] listen became bind")
    c.ok(s.get("timeout_ms") == 45000 and "timeout" not in s, f"[server] timeout became timeout_ms = 45000 (timeout_ms is {s.get('timeout_ms')!r})")
    f = new.get("log", {}).get("file")
    c.ok(f == {"path": orig["log"]["file"], "keep_days": 14}, f"[log.file] has path and keep_days = 14 (found {f!r})")
    for section in ("database", "cache"):
        c.ok(new.get(section) == expected[section], f"[{section}] is unchanged")
    c.ok(new.get("upstream") == expected["upstream"], "the [[upstream]] entries are unchanged (their timeouts stay in seconds)")
    if new == expected:
        c.note("the whole file reads as the expected version 3 settings")
    else:
        extra = sorted(set(new) ^ set(expected))
        c.fail("the settings differ from the expected version 3 file" + (f" (top-level keys: {extra})" if extra else ""))

    # Comments stay, except the ones right above the keys that moved.
    lines = orig_src.splitlines()
    moved = {"listen", "timeout", "file"}
    section = None
    exempt = set()
    for i, line in enumerate(lines):
        st = line.strip()
        if st.startswith("["):
            section = st
        key = st.split("=", 1)[0].strip() if "=" in st and not st.startswith("#") else None
        if key in moved and section in ("[server]", "[log]"):
            j = i - 1
            while j >= 0 and lines[j].strip().startswith("#"):
                exempt.add(j)
                j -= 1
    have = {line.strip() for line in src.splitlines()}
    lost = [lines[i].strip() for i in range(len(lines)) if lines[i].strip().startswith("#") and i not in exempt and lines[i].strip() not in have]
    c.ok(not lost, "the other comments are still there" if not lost else f"comments lost: {lost}")
c.done()
