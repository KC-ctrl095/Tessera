"""Settings from config/dev.json (or the file named by PARCELHUB_CONFIG)."""

import json
import os
from dataclasses import dataclass
from pathlib import Path


@dataclass
class Config:
    path: Path
    host: str
    port: int
    data_path: Path
    page_size: int
    max_page_size: int


def load(path=None):
    path = Path(path or os.environ.get("PARCELHUB_CONFIG") or "config/dev.json").resolve()
    with open(path, encoding="utf-8") as f:
        raw = json.load(f)
    base = path.parent.parent  # paths in the config are relative to the project folder
    return Config(
        path=path,
        host=raw.get("host", "127.0.0.1"),
        port=int(raw["port"]),
        data_path=(base / raw.get("data", "data/parcels.json")).resolve(),
        page_size=int(raw.get("page_size", 20)),
        max_page_size=int(raw.get("max_page_size", 100)),
    )
