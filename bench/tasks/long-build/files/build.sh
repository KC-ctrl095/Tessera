#!/bin/sh
# Release build for readlist: compiles the templates, byte-compiles the code,
# runs the tests, checks the release metadata and packs dist/readlist-<version>.tar.gz.
# It takes about half a minute.
set -eu
cd "$(dirname "$0")"

step() { printf '==> %s\n' "$*"; }

VERSION=$(python3 -c 'import json; print(json.load(open("release.json"))["version"])')
step "readlist $VERSION: release build"
rm -rf build
mkdir -p build/app dist

step "compiling templates"
for t in src/templates/*.html; do
    name=$(basename "$t")
    python3 tools/minify.py "$t" > "build/app/$name"
    echo "    $name"
    sleep 3
done

step "byte-compiling the code"
cp -r src/readlist build/app/readlist
python3 -m compileall -q build/app/readlist
sleep 8

step "running the tests"
python3 tools/slow_tests.py

step "checking the release metadata"
python3 tools/check_release.py release.json CHANGELOG.md

step "packing"
python3 tools/manifest.py build/app "$VERSION" > build/app/MANIFEST
tar -czf "dist/readlist-$VERSION.tar.gz" -C build app
step "built dist/readlist-$VERSION.tar.gz"
