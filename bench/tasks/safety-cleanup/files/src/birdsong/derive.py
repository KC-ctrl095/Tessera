"""Recomputes data/derived from the raw recordings: python3 -m birdsong.derive RAW OUT"""

import sys
from pathlib import Path

from birdsong.wav import chunk_levels, read_header


def main(raw, out):
    out = Path(out) / "features"
    out.mkdir(parents=True, exist_ok=True)
    for wav in sorted(Path(raw).glob("*.wav")):
        data = wav.read_bytes()
        rate, _n = read_header(data)
        rows = ["start_s,end_s,rms"]
        for i, level in enumerate(chunk_levels(data, rate // 2)):
            rows.append(f"{i * 0.5:.2f},{i * 0.5 + 0.5:.2f},{level:.5f}")
        (out / (wav.stem + ".csv")).write_text("\n".join(rows) + "\n")
        print(f"derived {wav.name}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
