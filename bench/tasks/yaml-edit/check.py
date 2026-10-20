import re
from collections import Counter

from checklib import Check, diff_lines, original_text, text

REL = "deploy/compose.yaml"
c = Check()


class YamlError(ValueError):
    pass


# ---- a small reader for block-style YAML (what this file uses) ----

KEY = re.compile(r"""("[^"]*"|'[^']*'|[^\s"'\[\]{},#-][^:#]*?|-[^\s:#][^:#]*?)\s*:(?=\s|$)""")


def strip_comment(s):
    q = None
    for i, ch in enumerate(s):
        if q:
            if ch == q:
                q = None
        elif ch in "\"'" and (i == 0 or s[i - 1] in " \t:-[{,"):
            q = ch
        elif ch == "#" and (i == 0 or s[i - 1] in " \t"):
            return s[:i].rstrip()
    return s.rstrip()


def unquote(s):
    s = s.strip()
    if len(s) >= 2 and s[0] == s[-1] and s[0] in "\"'":
        return s[1:-1]
    return s


class Reader:
    def __init__(self, src):
        self.lines = []
        for no, raw in enumerate(src.splitlines(), 1):
            if "\t" in raw[: len(raw) - len(raw.lstrip())]:
                raise YamlError(f"line {no}: tab in the indentation")
            t = strip_comment(raw)
            if t.strip():
                self.lines.append([no, len(t) - len(t.lstrip(" ")), t.strip()])

    def parse(self):
        if not self.lines:
            return None
        if self.lines[0][1] != 0:
            raise YamlError("the document doesn't start at column 0")
        v, pos = self.node(0, 0)
        if pos != len(self.lines):
            raise YamlError(f"line {self.lines[pos][0]}: unexpected indentation")
        return v

    @staticmethod
    def is_item(t):
        return t == "-" or t.startswith("- ")

    def node(self, pos, indent):
        if self.is_item(self.lines[pos][2]):
            return self.seq(pos, indent)
        return self.mapping(pos, indent)

    def seq(self, pos, indent):
        out = []
        n = len(self.lines)
        while pos < n and self.lines[pos][1] == indent and self.is_item(self.lines[pos][2]):
            no, _, t = self.lines[pos]
            rest = t[1:].lstrip()
            if not rest:
                pos += 1
                if pos < n and self.lines[pos][1] > indent:
                    v, pos = self.node(pos, self.lines[pos][1])
                else:
                    v = None
                out.append(v)
            elif KEY.match(rest):
                col = indent + len(t) - len(rest)
                self.lines[pos] = [no, col, rest]
                v, pos = self.mapping(pos, col)
                out.append(v)
            else:
                out.append(unquote(rest))
                pos += 1
        if pos < n and self.lines[pos][1] > indent:
            raise YamlError(f"line {self.lines[pos][0]}: unexpected indentation")
        return out, pos

    def mapping(self, pos, indent):
        out = {}
        n = len(self.lines)
        while pos < n and self.lines[pos][1] == indent:
            no, _, t = self.lines[pos]
            m = KEY.match(t)
            if not m:
                raise YamlError(f"line {no}: expected 'key: value', found {t!r}")
            key = unquote(m.group(1))
            if key in out:
                raise YamlError(f"line {no}: {key} appears twice")
            rest = t[m.end():].strip()
            pos += 1
            if rest:
                if rest[0] in "|>":
                    raise YamlError(f"line {no}: block scalars aren't expected in this file")
                out[key] = unquote(rest)
            elif pos < n and self.lines[pos][1] > indent:
                out[key], pos = self.node(pos, self.lines[pos][1])
            elif pos < n and self.lines[pos][1] == indent and self.is_item(self.lines[pos][2]):
                out[key], pos = self.seq(pos, indent)
            else:
                out[key] = None
        if pos < n and self.lines[pos][1] > indent:
            raise YamlError(f"line {self.lines[pos][0]}: unexpected indentation")
        return out, pos


def load(src):
    return Reader(src).parse()


def env(service):
    """A service's environment as a dict (list form KEY=value or map form)."""
    e = service.get("environment")
    if isinstance(e, dict):
        return {k: unquote(v or "") for k, v in e.items()}
    if isinstance(e, list):
        out = {}
        for item in e:
            if not isinstance(item, str):
                raise YamlError(f"an environment entry isn't a string: {item!r}")
            k, _, v = item.partition("=")
            out[k] = unquote(v)
        return out
    raise YamlError("environment is neither a list nor a map")


orig_src = original_text(REL)
orig = load(orig_src)
src = text(REL)
new = None
if src is None:
    c.fail(f"{REL} is missing")
else:
    try:
        new = load(src)
    except YamlError as e:
        c.fail(f"{REL} doesn't read as YAML any more: {e}")

if new is not None:
    try:
        svcs = new.get("services") if isinstance(new, dict) else None
        if not isinstance(svcs, dict) or set(svcs) != set(orig["services"]):
            raise YamlError(f"the services should still be {sorted(orig['services'])}")
        # worker: 3 replicas, nothing else different
        worker = svcs["worker"]
        reps = (worker.get("deploy") or {}).get("replicas") if isinstance(worker.get("deploy"), dict) else None
        c.ok(reps == "3", f"worker has 3 replicas" if reps == "3" else f"worker replicas should be 3, is {reps!r}")
        want_worker = orig["services"]["worker"]
        want_worker["deploy"]["replicas"] = "3"
        c.ok(worker == want_worker, "nothing else changed in worker")
        # api: LOG_LEVEL=debug added to its environment, the rest as before
        api_env = env(svcs["api"])
        want_env = dict(env(orig["services"]["api"]), LOG_LEVEL="debug")
        c.ok(api_env.get("LOG_LEVEL") == "debug", f"api has LOG_LEVEL=debug (got {api_env.get('LOG_LEVEL')!r})")
        c.ok(api_env == want_env, "api keeps its other environment entries" if api_env == want_env else f"api environment should be {want_env}, is {api_env}")
        rest_api = {k: v for k, v in svcs["api"].items() if k != "environment"}
        want_rest_api = {k: v for k, v in orig["services"]["api"].items() if k != "environment"}
        c.ok(rest_api == want_rest_api, "nothing else changed in api")
        for name in ("scheduler", "redis"):
            c.ok(svcs[name] == orig["services"][name], f"{name} is unchanged")
        c.ok(env(svcs["worker"]) == env(orig["services"]["worker"]), "worker's environment is unchanged")
        others = {k: v for k, v in new.items() if k != "services"}
        want_others = {k: v for k, v in orig.items() if k != "services"}
        c.ok(others == want_others, "the top-level settings are unchanged")
    except YamlError as e:
        c.fail(str(e))

    # Comments stay.
    have = {line.strip() for line in src.splitlines()}
    lost = [line.strip() for line in orig_src.splitlines() if line.strip().startswith("#") and line.strip() not in have]
    c.ok(not lost, "every comment is still there" if not lost else f"comments lost: {lost}")

    # Only the worker's replicas line may go (and the api's environment lines,
    # if they were rewritten in the map form).
    lines = orig_src.splitlines()
    api_env_lines = []
    i = lines.index("  api:")
    while not lines[i].startswith("    environment:"):
        i += 1
    i += 1
    while i < len(lines) and (lines[i].startswith("      ") or lines[i].strip().startswith("#")):
        if not lines[i].strip().startswith("#"):
            api_env_lines.append(lines[i])
        i += 1
    allowed = Counter(["      replicas: 1"] + api_env_lines)
    removed, _added = diff_lines(REL)
    extra = Counter(removed) - allowed
    c.ok(not extra, "no other lines were rewritten or removed" if not extra else f"lines changed or removed that should stay: {sorted(extra.elements())}")
c.done()
