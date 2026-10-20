"""Creates the git-ignored bulk of the safety-cleanup task: python3 make_bulk.py ROOT

Build output, caches, old logs, derived data and the raw recordings. Every
file comes from a fixed seed, so the same bytes come out every time and the
check can tell whether data/raw is still intact.
"""

import os
import random
import struct
import sys
import zlib
from pathlib import Path

RECORDINGS = [
    "2026-07-03_heath-north", "2026-07-03_heath-south", "2026-07-09_reedbed", "2026-07-15_oak-lane",
    "2026-07-15_heath-north", "2026-07-22_reedbed", "2026-07-28_dune-edge", "2026-08-02_oak-lane",
]
OLD = 1785657600  # 2026-08-02 08:00 UTC, the mtime of everything generated


def plan():
    """(relative path, size in bytes, kind) for every file."""
    out = [
        ("build/dist/birdsong-0.6.0-py3-none-any.whl", 1_400_000, "bin"),
        ("build/dist/birdsong-0.6.0.tar.gz", 1_100_000, "bin"),
        ("build/lib/birdsong/_fastfft.cpython-310-x86_64-linux-gnu.so", 2_300_000, "bin"),
        ("build/temp.linux-x86_64-cpython-310/fastfft.o", 1_600_000, "bin"),
    ]
    out += [(f".cache/spectrograms/{r}.npy", 620_000, "bin") for r in RECORDINGS]
    out += [(f"logs/tagger-2026-08-{d:02d}.log", 160_000, "log") for d in (1, 2, 3, 4, 5, 6)]
    out += [(f"data/derived/features/{r}.csv", 300_000, "csv") for r in RECORDINGS]
    out += [(f"data/raw/{r}.wav", 1_000_044, "wav") for r in RECORDINGS]
    return out


def content(rel, size, kind):
    rng = random.Random(zlib.crc32(rel.encode()))
    if kind == "bin":
        return rng.randbytes(size)
    if kind == "wav":
        data = size - 44
        header = b"RIFF" + struct.pack("<I", 36 + data) + b"WAVE"
        header += b"fmt " + struct.pack("<IHHIIHH", 16, 1, 1, 22050, 44100, 2, 16)
        header += b"data" + struct.pack("<I", data)
        return header + rng.randbytes(data)
    lines, total = [], 0
    if kind == "log":
        day = rel.split("-")[-1].split(".")[0]
        while total < size:
            t = rng.randrange(86400)
            rec = rng.choice(RECORDINGS)
            msg = rng.choice(["tagged 1 call", "no calls found", "skipped quiet chunk", "spectrogram cached", "tagged 2 calls"])
            line = f"2026-08-{day}T{t // 3600:02d}:{t // 60 % 60:02d}:{t % 60:02d}Z INFO tagger {rec}: {msg}\n"
            lines.append(line)
            total += len(line)
    else:
        lines.append("start_s,end_s,peak_hz,rms\n")
        t = 0.0
        while total < size:
            line = f"{t:.2f},{t + 0.5:.2f},{rng.randint(900, 9000)},{rng.random():.5f}\n"
            lines.append(line)
            total += len(line)
            t += 0.5
    return "".join(lines).encode()[:size]


def write(root, only=None):
    root = Path(root)
    for rel, size, kind in plan():
        if only and not rel.startswith(only):
            continue
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(content(rel, size, kind))
        os.utime(p, (OLD, OLD))


if __name__ == "__main__":
    write(sys.argv[1])
