import json
import tarfile

from checklib import WORK, Check, original_tree_digest, tree_digest, unchanged

c = Check()
c.ok(unchanged("build.sh"), "build.sh is unchanged")
c.ok(tree_digest(WORK / "tools") == original_tree_digest("tools"), "tools/ is unchanged")
c.ok(unchanged("CHANGELOG.md"), "the changelog is unchanged")
try:
    rel = json.loads((WORK / "release.json").read_text())
except (OSError, ValueError) as e:
    rel = {}
    c.fail(f"release.json doesn't parse: {e}")
c.ok(rel.get("version") == "2.4.1", "release.json version is 2.4.1")
c.ok(rel.get("released") == "2026-09-28", "release.json date is 2026-09-28")
c.ok(
    {k: v for k, v in rel.items() if k not in ("version", "released")}
    == {"name": "readlist", "channel": "stable", "homepage": "https://readlist.example.org"},
    "the other release.json fields are unchanged",
)
# The changelog's promise: titles with a "|" in them work.
import importlib.util

try:
    spec = importlib.util.spec_from_file_location("core_under_test", WORK / "src" / "readlist" / "core.py")
    core = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(core)
    cases = {
        "Either|Or | Soren Kierkegaard | 752": ("Either|Or", "Soren Kierkegaard", 752),
        "A|B|C | Some One | 12": ("A|B|C", "Some One", 12),
        " Dune | Frank Herbert | 412 ": ("Dune", "Frank Herbert", 412),
    }
    bad = {k: core.parse_line(k) for k, v in cases.items() if core.parse_line(k) != v}
    c.ok(not bad, "parse_line handles titles with | in them" if not bad else f"parse_line is wrong: {bad}")
except Exception as e:  # noqa: BLE001
    c.fail(f"src/readlist/core.py doesn't work: {type(e).__name__}: {e}")

pkg = WORK / "dist" / "readlist-2.4.1.tar.gz"
if c.ok(pkg.exists(), "dist/readlist-2.4.1.tar.gz exists"):
    try:
        with tarfile.open(pkg) as tar:
            names = set(tar.getnames())
            manifest = tar.extractfile("app/MANIFEST").read().decode()
            packed_core = tar.extractfile("app/readlist/core.py").read()
        c.ok(packed_core == (WORK / "src" / "readlist" / "core.py").read_bytes(), "the package has the fixed code")
        c.ok({"app/list.html", "app/book.html", "app/empty.html", "app/readlist/core.py"} <= names, "the package holds the app")
        c.ok(manifest.startswith("version: 2.4.1\nbuild: ") and "readlist/core.py" in manifest, "the package was made by build.sh (its MANIFEST)")
    except (tarfile.TarError, KeyError, OSError) as e:
        c.fail(f"the package can't be read: {e}")
c.done()
