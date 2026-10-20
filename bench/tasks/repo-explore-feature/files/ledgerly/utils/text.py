"""Text helpers."""

import unicodedata


def slug(s: str) -> str:
    s = unicodedata.normalize("NFKD", s).encode("ascii", "ignore").decode()
    return "-".join("".join(ch if ch.isalnum() else " " for ch in s.lower()).split())


def truncate(s: str, n: int) -> str:
    return s if len(s) <= n else s[: n - 1] + "…"
