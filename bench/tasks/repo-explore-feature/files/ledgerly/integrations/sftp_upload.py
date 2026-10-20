"""Uploads exports to the accountant's server (here: copies into a drop folder)."""

import shutil
from pathlib import Path


def upload(path, drop=Path("drop")):
    drop.mkdir(parents=True, exist_ok=True)
    return Path(shutil.copy(path, drop / Path(path).name))
