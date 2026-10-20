#!/usr/bin/env python3
"""Fail closed before publishing a Linux, Windows and Android launch release.

Needs 7z (NSIS extraction), apksigner and aapt (Android SDK build-tools) on PATH.
Does not execute downloaded programs or installers. SHA256SUMS is integrity, not
publisher authentication. Optionally pin TESSERA_ANDROID_CERT_SHA256.
"""
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile

REQUIRED = (
    "tessera-linux-x86_64.tar.gz", "tessera-linux-aarch64.tar.gz",
    "tessera-desktop-linux-x86_64.tar.gz", "tessera-windows-x86_64.zip",
    "tessera-desktop-windows-x86_64-setup.exe", "tessera-android-arm64.apk",
    "install.sh", "install.ps1",
)
BUNDLE = rb"index-[A-Za-z0-9_-]+\.js"


def require(ok, message):
    if not ok:
        raise ValueError(message)


def command(*args):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False)
    require(result.returncode == 0, f"{args[0]} failed inspecting {args[-1]}\n{result.stdout.decode(errors='replace')}")
    return result.stdout.decode(errors="replace")


def ui(data, name):
    require(re.search(BUNDLE, data), f"{name} has no embedded UI index-*.js bundle")


def tar_member(file, name):
    with tarfile.open(file) as archive:
        matches = [m for m in archive.getmembers() if m.name.removeprefix("./") == name and m.isfile()]
        require(len(matches) == 1, f"{file.name} must contain exactly one {name}")
        return archive.extractfile(matches[0]).read()


def check_apk(file):
    # A zip entry named *.RSA is not proof of signing. Verify the APK signature.
    output = command("apksigner", "verify", "--verbose", "--print-certs", str(file))
    require("CN=Android Debug" not in output and "CN=Tessera sideload" not in output,
            "APK uses a development/sideload certificate, not a public release identity")
    pin = os.environ.get("TESSERA_ANDROID_CERT_SHA256", "").replace(":", "").lower()
    if pin:
        fingerprints = re.findall(r"certificate SHA-256 digest: ([0-9a-fA-F]+)", output)
        require(pin in [f.lower() for f in fingerprints], "APK signing certificate does not match TESSERA_ANDROID_CERT_SHA256")
    manifest = command("aapt", "dump", "badging", str(file))
    require("application-debuggable" not in manifest, "APK is debuggable")
    require("package: name='dev.tessera.app'" in manifest, "APK has the wrong application ID")
    with zipfile.ZipFile(file) as archive:
        ui(archive.read("lib/arm64-v8a/libtessera_app_lib.so"), file.name)


def check(folder):
    folder = folder.resolve()
    require(folder.is_dir(), f"Not an artifacts folder: {folder}")
    for name in (*REQUIRED, "SHA256SUMS"):
        file = folder / name
        require(file.is_file() and not file.is_symlink() and file.stat().st_size > 0, f"Missing or empty artifact: {name}")
    sums = {}
    for line in (folder / "SHA256SUMS").read_text().splitlines():
        match = re.fullmatch(r"([0-9a-fA-F]{64}) [ *]([^/\\]+)", line)
        require(match is not None, f"Invalid SHA256SUMS line: {line!r}")
        digest, name = match.groups()
        require(name not in sums and name != "SHA256SUMS" and name not in (".", ".."), f"Duplicate/invalid checksum entry: {name}")
        sums[name] = digest.lower()
    files = {p.name for p in folder.iterdir() if p.name != "SHA256SUMS"}
    require(files == set(sums), "SHA256SUMS must cover every artifact exactly once")
    for name, digest in sums.items():
        file = folder / name
        require(file.is_file() and not file.is_symlink(), f"Not a regular artifact: {name}")
        with file.open("rb") as stream:
            hasher = hashlib.sha256()
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                hasher.update(chunk)
            actual = hasher.hexdigest()
        require(actual == digest, f"Checksum mismatch: {name}")
    for arch in ("x86_64", "aarch64"):
        file = folder / f"tessera-linux-{arch}.tar.gz"
        ui(tar_member(file, "tessera"), file.name)
    file = folder / "tessera-desktop-linux-x86_64.tar.gz"
    ui(tar_member(file, "tessera-desktop"), file.name)
    with zipfile.ZipFile(folder / "tessera-windows-x86_64.zip") as archive:
        helpers = {name: archive.read(name) for name in ("tessera.exe", "tessera-service.exe")}
        ui(helpers["tessera.exe"], "Windows CLI")
        require(all(helpers.values()), "Empty Windows helper")
    require(shutil.which("7z"), "Install 7z to inspect the Windows installer (no strings-only bypass)")
    with tempfile.TemporaryDirectory(prefix="tessera-release-check-") as tmp:
        command("7z", "x", "-y", f"-o{tmp}", str(folder / "tessera-desktop-windows-x86_64-setup.exe"))
        for name, data in helpers.items():
            # Tauri NSIS places mapped resources at the install root.
            file = Path(tmp) / name
            require(file.is_file() and file.read_bytes() == data, f"Windows installer must contain the packaged {name} at its root")
    require(shutil.which("apksigner") and shutil.which("aapt"), "Install Android SDK build-tools and add apksigner and aapt to PATH")
    check_apk(folder / "tessera-android-arm64.apk")
    print(f"Release complete. {len(sums)} artifacts and all checksums verified, CLI/desktop/APK UI found, Windows helpers matched, APK signature verified.")


if __name__ == "__main__":
    try:
        require(len(sys.argv) == 2, "Usage: python3 scripts/check-release.py ARTIFACTS_FOLDER")
        check(Path(sys.argv[1]))
    except (ValueError, OSError, KeyError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(f"Release incomplete: {error}", file=sys.stderr)
        sys.exit(1)
