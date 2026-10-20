"""Make README pictures small: each PNG in the given folder becomes a
256-colour PNG here (docs/images), at most 1600 px wide.

    python3 docs/images/shrink.py "$E2E_OUT/shots/readme"

Needs Pillow. Pictures of an interface keep their look at 256 colours and
come out at a fraction of the size."""

import os
import sys

from PIL import Image

src = sys.argv[1]
here = os.path.dirname(os.path.abspath(__file__))
for name in sorted(os.listdir(src)):
    if not name.endswith(".png"):
        continue
    im = Image.open(os.path.join(src, name)).convert("RGB")
    if im.width > 1600:
        im = im.resize((1600, round(im.height * 1600 / im.width)), Image.LANCZOS)
    out = os.path.join(here, name)
    im.quantize(colors=256, method=Image.MEDIANCUT, dither=Image.FLOYDSTEINBERG).save(out, optimize=True)
    print(f"{name}: {im.width}x{im.height}, {os.path.getsize(out) // 1024} KB")
