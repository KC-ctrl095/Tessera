"""tinytpl: the very small template language of our notification emails.

    {{ name }}                    a value, HTML-escaped
    {{ user.email }}              a key of a dict (or an attribute of an object)
    {{ tags|first|upper }}        filters, applied left to right
    {{ body|safe }}               no escaping
    {% for t in tags %}…{% endfor %}       loops; inside, loop.index (1-based),
                                           loop.first and loop.last
    {% if x %}…{% else %}…{% endif %}      conditions ("not x" works too)

Loops and conditions can be nested. A missing value renders as nothing.
Filters: upper, lower, trim, length, first, safe.
"""

import re
from collections import ChainMap

_TOKEN = re.compile(r"({{.*?}}|{%.*?%})", re.S)


class TemplateError(Exception):
    pass


def escape(value):
    """HTML-escape text: & < > " and '."""
    s = str(value)
    for raw, entity in (("<", "&lt;"), (">", "&gt;"), ('"', "&quot;"), ("'", "&#39;"), ("&", "&amp;")):
        s = s.replace(raw, entity)
    return s


FILTERS = {
    "upper": lambda v: str(v).upper(),
    "lower": lambda v: str(v).lower(),
    "trim": lambda v: str(v).strip(),
    "length": lambda v: len(v),
    "first": lambda v: v[0] if v else "",
}


def lookup(scope, expr):
    """The value of `a.b.c` in the scope (None when any part is missing)."""
    parts = expr.strip().split(".")
    value = scope.get(parts[0])
    for part in parts[1:]:
        if value is None:
            return None
        value = getattr(value, part, None)
    return value


def _tag(token):
    return token[2:-2].strip().split()


def _find(tokens, start, opener, closer, middle=None):
    """Index of the token closing the block opened at `start` (and of its
    `middle` tag, such as else, when there is one at the same depth)."""
    depth = 0
    middle_at = None
    for i in range(start + 1, len(tokens)):
        tok = tokens[i]
        if not tok.startswith("{%"):
            continue
        words = _tag(tok)
        if not words:
            continue
        if words[0] == closer:
            return i, middle_at
        elif middle and words[0] == middle and middle_at is None:
            middle_at = i
    raise TemplateError(f"no {{% {closer} %}} for {tokens[start]}")


def parse(tokens, start=0, end=None):
    end = len(tokens) if end is None else end
    nodes = []
    i = start
    while i < end:
        tok = tokens[i]
        if tok.startswith("{{"):
            expr, *filters = [p.strip() for p in tok[2:-2].split("|")]
            nodes.append(("var", expr, filters))
        elif tok.startswith("{%"):
            words = _tag(tok)
            if words and words[0] == "for" and len(words) == 4 and words[2] == "in":
                close, _ = _find(tokens, i, "for", "endfor")
                nodes.append(("for", words[1], words[3], parse(tokens, i + 1, close)))
                i = close
            elif words and words[0] == "if":
                close, other = _find(tokens, i, "if", "endif", "else")
                body = parse(tokens, i + 1, other if other is not None else close)
                orelse = parse(tokens, other + 1, close) if other is not None else []
                nodes.append(("if", " ".join(words[1:]), body, orelse))
                i = close
            else:
                raise TemplateError(f"unexpected tag {tok}")
        elif tok:
            nodes.append(("text", tok))
        i += 1
    return nodes


def _truthy(scope, expr):
    expr = expr.strip()
    if expr.startswith("not "):
        return not _truthy(scope, expr[4:])
    return bool(lookup(scope, expr))


def _render(nodes, scope):
    out = []
    for node in nodes:
        kind = node[0]
        if kind == "text":
            out.append(node[1])
        elif kind == "var":
            value = lookup(scope, node[1])
            safe = False
            for name in reversed(node[2]):
                if name == "safe":
                    safe = True
                    continue
                if name not in FILTERS:
                    raise TemplateError(f"unknown filter {name!r}")
                value = FILTERS[name]("" if value is None else value)
            text = "" if value is None else str(value)
            out.append(text if safe else escape(text))
        elif kind == "for":
            items = list(lookup(scope, node[2]) or [])
            for n, item in enumerate(items):
                loop = {"index": n + 1, "first": n == 0, "last": n == len(items)}
                out.append(_render(node[3], ChainMap({node[1]: item, "loop": loop}, scope)))
        elif kind == "if":
            out.append(_render(node[2] if _truthy(scope, node[1]) else node[3], scope))
    return "".join(out)


def render(template, context=None):
    """Render `template` with the values in `context` (a dict)."""
    tokens = _TOKEN.split(template)
    return _render(parse(tokens), ChainMap(dict(context or {})))
