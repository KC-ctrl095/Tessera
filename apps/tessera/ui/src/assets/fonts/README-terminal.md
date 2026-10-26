# Terminal font

Iosevka Term 33.3.1 by Renzhi Li, under the SIL Open Font License 1.1.
The full licence is in OFL-Iosevka.txt.

Source archive https://github.com/be5invis/Iosevka/releases/download/v33.3.1/PkgWebFont-IosevkaTerm-33.3.1.zip

Only Regular and Bold are bundled. Italic is synthesized by the renderer.
The subset keeps Latin, Greek, Cyrillic, combining marks, punctuation,
mathematical and geometric symbols, every box drawing and block character,
every braille pattern and all available Powerline symbols. Other scripts
fall back to the platform's fonts. Ligature layout features are dropped
because terminal cells must remain independent. Nothing is fetched remotely
at runtime. The CSS and fonts load only with TermView.

Reproduce with Python 3, fontTools 4.63.0 and Brotli installed. Run from this
folder with the source archive saved as iosevka.zip.

```python
import io
import zipfile
from fontTools import subset
from fontTools.ttLib import TTFont

with zipfile.ZipFile("iosevka.zip") as source:
    for weight in ["Regular", "Bold"]:
        font = TTFont(io.BytesIO(source.read(f"TTF/IosevkaTerm-{weight}.ttf")))
        options = subset.Options()
        options.flavor = "woff2"
        options.layout_features = []
        keep = subset.Subsetter(options=options)
        keep.populate(unicodes=(
            list(range(0x250)) + list(range(0x300, 0x530)) +
            list(range(0x1e00, 0x1f00)) + list(range(0x2000, 0x2c00)) +
            list(range(0xe0a0, 0xe0d8))
        ))
        keep.subset(font)
        font.flavor = "woff2"
        font.save(f"iosevka-term-{weight.lower()}.woff2")
```
