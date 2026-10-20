"""A small TOML reader for Python 3.10 (no `tomllib` there).

Uses `tomllib` when it exists. The fallback covers what task files and the
config files in tasks use: tables, arrays of tables, dotted keys, basic and
literal strings (also multi-line), integers, floats, booleans, arrays and
inline tables. Dates are read as plain strings.
"""

import re

try:  # Python 3.11+
    import tomllib as _tomllib
except ImportError:  # pragma: no cover - depends on the interpreter
    _tomllib = None


class TomlError(ValueError):
    pass


_ESCAPES = {"b": "\b", "t": "\t", "n": "\n", "f": "\f", "r": "\r", '"': '"', "\\": "\\"}
_BARE = re.compile(r"[A-Za-z0-9_-]+")
_NUMBER = re.compile(r"[+-]?(0x[0-9A-Fa-f_]+|0o[0-7_]+|0b[01_]+|inf|nan|[0-9_]+(\.[0-9_]+)?([eE][+-]?[0-9_]+)?)")
_DATE = re.compile(r"\d{4}-\d{2}-\d{2}([T ]\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}:\d{2})?)?|\d{2}:\d{2}:\d{2}(\.\d+)?")


class _Parser:
    def __init__(self, text):
        self.s = text
        self.i = 0

    def error(self, msg):
        line = self.s.count("\n", 0, self.i) + 1
        raise TomlError(f"line {line}: {msg}")

    def peek(self, n=1):
        return self.s[self.i:self.i + n]

    def skip_ws(self):
        while self.i < len(self.s) and self.s[self.i] in " \t":
            self.i += 1

    def skip_comment(self):
        if self.peek() == "#":
            while self.i < len(self.s) and self.s[self.i] != "\n":
                self.i += 1

    def skip_ws_nl(self):
        """Whitespace, newlines and comments (inside arrays)."""
        while self.i < len(self.s):
            c = self.s[self.i]
            if c in " \t\r\n":
                self.i += 1
            elif c == "#":
                self.skip_comment()
            else:
                break

    def end_of_line(self):
        self.skip_ws()
        self.skip_comment()
        if self.peek() == "\r":
            self.i += 1
        if self.i < len(self.s) and self.peek() != "\n":
            self.error("expected the end of the line")
        self.i += 1

    def key(self):
        parts = []
        while True:
            self.skip_ws()
            c = self.peek()
            if c == '"':
                parts.append(self.basic_string())
            elif c == "'":
                parts.append(self.literal_string())
            else:
                m = _BARE.match(self.s, self.i)
                if not m:
                    self.error("expected a key")
                parts.append(m.group(0))
                self.i = m.end()
            self.skip_ws()
            if self.peek() == ".":
                self.i += 1
                continue
            return parts

    def basic_string(self):
        if self.peek(3) == '"""':
            self.i += 3
            if self.peek() == "\n":
                self.i += 1
            out = []
            while True:
                if self.i >= len(self.s):
                    self.error("unterminated string")
                if self.peek(3) == '"""':
                    # Up to two quotes may end the content.
                    extra = 0
                    while self.peek(4 + extra)[3 + extra:] == '"' and extra < 2:
                        extra += 1
                    out.append('"' * extra)
                    self.i += 3 + extra
                    return "".join(out)
                c = self.s[self.i]
                if c == "\\":
                    nxt = self.s[self.i + 1:self.i + 2]
                    if nxt in "\n \t\r" and nxt:
                        # Line-ending backslash: trim the whitespace that follows.
                        j = self.i + 1
                        while j < len(self.s) and self.s[j] in " \t\r\n":
                            j += 1
                        self.i = j
                        continue
                    out.append(self.escape())
                else:
                    out.append(c)
                    self.i += 1
        self.i += 1
        out = []
        while True:
            if self.i >= len(self.s) or self.s[self.i] == "\n":
                self.error("unterminated string")
            c = self.s[self.i]
            if c == '"':
                self.i += 1
                return "".join(out)
            if c == "\\":
                out.append(self.escape())
            else:
                out.append(c)
                self.i += 1

    def escape(self):
        nxt = self.s[self.i + 1:self.i + 2]
        if nxt in _ESCAPES:
            self.i += 2
            return _ESCAPES[nxt]
        if nxt in ("u", "U"):
            n = 4 if nxt == "u" else 8
            hexs = self.s[self.i + 2:self.i + 2 + n]
            self.i += 2 + n
            return chr(int(hexs, 16))
        self.error(f"bad escape \\{nxt}")

    def literal_string(self):
        if self.peek(3) == "'''":
            self.i += 3
            if self.peek() == "\n":
                self.i += 1
            end = self.s.find("'''", self.i)
            if end < 0:
                self.error("unterminated string")
            while self.s[end + 3:end + 4] == "'":
                end += 1
            out = self.s[self.i:end]
            self.i = end + 3
            return out
        self.i += 1
        end = self.s.find("'", self.i)
        if end < 0 or "\n" in self.s[self.i:end]:
            self.error("unterminated string")
        out = self.s[self.i:end]
        self.i = end + 1
        return out

    def value(self):
        self.skip_ws()
        c = self.peek()
        if c == '"':
            return self.basic_string()
        if c == "'":
            return self.literal_string()
        if c == "[":
            return self.array()
        if c == "{":
            return self.inline_table()
        if self.s.startswith("true", self.i):
            self.i += 4
            return True
        if self.s.startswith("false", self.i):
            self.i += 5
            return False
        m = _DATE.match(self.s, self.i)
        if m and (len(m.group(0)) >= 8):
            self.i = m.end()
            return m.group(0)
        m = _NUMBER.match(self.s, self.i)
        if m:
            self.i = m.end()
            t = m.group(0).replace("_", "")
            low = t.lower().lstrip("+-")
            if low.startswith(("0x", "0o", "0b")):
                sign = -1 if t.startswith("-") else 1
                return sign * int(low, 0)
            if low in ("inf", "nan") or any(ch in t for ch in ".eE"):
                return float(t)
            return int(t)
        self.error("expected a value")

    def array(self):
        self.i += 1
        out = []
        while True:
            self.skip_ws_nl()
            if self.peek() == "]":
                self.i += 1
                return out
            out.append(self.value())
            self.skip_ws_nl()
            if self.peek() == ",":
                self.i += 1
            elif self.peek() == "]":
                self.i += 1
                return out
            else:
                self.error("expected , or ] in an array")

    def inline_table(self):
        self.i += 1
        out = {}
        self.skip_ws()
        if self.peek() == "}":
            self.i += 1
            return out
        while True:
            parts = self.key()
            if self.peek() != "=":
                self.error("expected =")
            self.i += 1
            _assign(out, parts, self.value(), self)
            self.skip_ws()
            if self.peek() == ",":
                self.i += 1
            elif self.peek() == "}":
                self.i += 1
                return out
            else:
                self.error("expected , or } in an inline table")

    def parse(self):
        root = {}
        current = root
        while True:
            self.skip_ws_nl()
            if self.i >= len(self.s):
                return root
            if self.peek(2) == "[[":
                self.i += 2
                parts = self.key()
                if self.peek(2) != "]]":
                    self.error("expected ]]")
                self.i += 2
                parent = _walk(root, parts[:-1], self)
                arr = parent.setdefault(parts[-1], [])
                if not isinstance(arr, list):
                    self.error(f"{'.'.join(parts)} is not an array of tables")
                current = {}
                arr.append(current)
                self.end_of_line()
            elif self.peek() == "[":
                self.i += 1
                parts = self.key()
                if self.peek() != "]":
                    self.error("expected ]")
                self.i += 1
                current = _walk(root, parts, self)
                self.end_of_line()
            else:
                parts = self.key()
                if self.peek() != "=":
                    self.error("expected =")
                self.i += 1
                _assign(current, parts, self.value(), self)
                self.end_of_line()


def _walk(table, parts, p):
    for part in parts:
        nxt = table.setdefault(part, {})
        if isinstance(nxt, list):
            nxt = nxt[-1]
        if not isinstance(nxt, dict):
            p.error(f"{part} is not a table")
        table = nxt
    return table


def _assign(table, parts, value, p):
    t = _walk(table, parts[:-1], p)
    if parts[-1] in t:
        p.error(f"duplicate key {'.'.join(parts)}")
    t[parts[-1]] = value


def loads(text):
    """Parse TOML text into dicts and lists."""
    if _tomllib is not None:
        try:
            return _tomllib.loads(text)
        except _tomllib.TOMLDecodeError as e:
            raise TomlError(str(e)) from e
    if text.startswith("﻿"):
        text = text[1:]
    if not text.endswith("\n"):
        text += "\n"
    return _Parser(text).parse()


def load_file(path):
    with open(path, encoding="utf-8") as f:
        return loads(f.read())
