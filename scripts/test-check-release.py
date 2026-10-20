#!/usr/bin/env python3
"""Small synthetic packaging fixtures. Never run a release binary or use keys."""
import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location("release_check", Path(__file__).with_name("check-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
UI = b"fixture binary with assets/index-fixture123.js"


class ReleaseCheck(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)
        for name in release.REQUIRED:
            (self.folder / name).write_bytes(b"fixture")
        for arch in ("x86_64", "aarch64"):
            self.tar(f"tessera-linux-{arch}.tar.gz", "tessera", UI)
        self.tar("tessera-desktop-linux-x86_64.tar.gz", "tessera-desktop", UI)
        self.zip("tessera-windows-x86_64.zip", {"tessera.exe": UI, "tessera-service.exe": b"helper"})
        self.zip("tessera-android-arm64.apk", {"lib/arm64-v8a/libtessera_app_lib.so": UI})
        self.sums()
        self.output = "Signer #1 certificate DN: CN=Tessera Public\nSigner #1 certificate SHA-256 digest: " + "ab" * 32
        self.manifest = "package: name='dev.tessera.app' versionCode='1000' versionName='0.1.0'"
        self.helper = b"helper"
        self.addCleanup(patch.stopall)
        patch.object(release.shutil, "which", return_value="fixture-tool").start()
        patch.object(release, "command", side_effect=self.command).start()
        patch.dict(release.os.environ, {"TESSERA_ANDROID_CERT_SHA256": ""}).start()

    def tar(self, file, name, data):
        with tarfile.open(self.folder / file, "w:gz") as archive:
            item = tarfile.TarInfo(name)
            item.size = len(data)
            archive.addfile(item, io.BytesIO(data))

    def zip(self, file, entries):
        with zipfile.ZipFile(self.folder / file, "w") as archive:
            for name, data in entries.items():
                archive.writestr(name, data)

    def sums(self):
        text = "".join(f"{hashlib.sha256(f.read_bytes()).hexdigest()}  {f.name}\n"
                       for f in sorted(self.folder.iterdir()) if f.name != "SHA256SUMS")
        (self.folder / "SHA256SUMS").write_text(text)

    def command(self, *args):
        if args[0] == "7z":
            folder = Path(next(a[2:] for a in args if a.startswith("-o")))
            (folder / "tessera.exe").write_bytes(UI)
            if self.helper is not None:
                (folder / "tessera-service.exe").write_bytes(self.helper)
            return "fixture NSIS extraction"
        if args[0] == "apksigner":
            return self.output
        if args[0] == "aapt":
            return self.manifest
        raise AssertionError(args)

    def test_complete_release(self):
        release.check(self.folder)

    def test_each_launch_artifact_is_required(self):
        for name in (*release.REQUIRED, "SHA256SUMS"):
            with self.subTest(name=name):
                file = self.folder / name
                data = file.read_bytes()
                file.unlink()
                with self.assertRaisesRegex(ValueError, "Missing or empty"):
                    release.check(self.folder)
                file.write_bytes(data)

    def test_corrupted_checksum(self):
        (self.folder / "install.sh").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
            release.check(self.folder)

    def test_unlisted_artifact(self):
        (self.folder / "extra.apk").write_bytes(b"extra")
        with self.assertRaisesRegex(ValueError, "cover every artifact"):
            release.check(self.folder)

    def test_duplicate_checksum(self):
        file = self.folder / "SHA256SUMS"
        file.write_text(file.read_text() * 2)
        with self.assertRaisesRegex(ValueError, "Duplicate"):
            release.check(self.folder)

    def test_cli_without_ui(self):
        self.tar("tessera-linux-aarch64.tar.gz", "tessera", b"CLI without embedded web files")
        self.sums()
        with self.assertRaisesRegex(ValueError, "no embedded UI"):
            release.check(self.folder)

    def test_missing_installer_helper(self):
        self.helper = None
        with self.assertRaisesRegex(ValueError, "tessera-service.exe"):
            release.check(self.folder)

    def test_stale_installer_helper(self):
        self.helper = b"different build"
        with self.assertRaisesRegex(ValueError, "tessera-service.exe"):
            release.check(self.folder)

    def test_no_7z_is_not_a_bypass(self):
        with patch.object(release.shutil, "which", return_value=None):
            with self.assertRaisesRegex(ValueError, "Install 7z"):
                release.check(self.folder)

    def test_unsigned_apk(self):
        original = self.command
        def command(*args):
            if args[0] == "apksigner":
                raise ValueError("apksigner failed")
            return original(*args)
        with patch.object(release, "command", side_effect=command):
            with self.assertRaisesRegex(ValueError, "apksigner failed"):
                release.check(self.folder)

    def test_development_certificates(self):
        for name in ("Android Debug", "Tessera sideload"):
            self.output = f"Signer #1 certificate DN: CN={name}"
            with self.assertRaisesRegex(ValueError, "development/sideload"):
                release.check(self.folder)

    def test_debuggable_apk(self):
        self.manifest += "\napplication-debuggable"
        with self.assertRaisesRegex(ValueError, "debuggable"):
            release.check(self.folder)

    def test_wrong_signing_identity(self):
        with patch.dict(release.os.environ, {"TESSERA_ANDROID_CERT_SHA256": "cd" * 32}):
            with self.assertRaisesRegex(ValueError, "does not match"):
                release.check(self.folder)

    def test_apk_without_ui(self):
        self.zip("tessera-android-arm64.apk", {"lib/arm64-v8a/libtessera_app_lib.so": b"no UI"})
        self.sums()
        with self.assertRaisesRegex(ValueError, "no embedded UI"):
            release.check(self.folder)


if __name__ == "__main__":
    unittest.main()
