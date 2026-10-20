"""Shared test helpers: a throwaway notes folder and a CLI runner."""

import contextlib
import io
import tempfile
import unittest
from pathlib import Path

from notekit.__main__ import main


class NotesTestCase(unittest.TestCase):
    def make_notes(self, notes):
        """A temporary notes folder holding {name: text} as name.md files; returns its path."""
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        for name, text in notes.items():
            Path(tmp.name, name + ".md").write_text(text, encoding="utf-8")
        return tmp.name

    def run_cli(self, *argv):
        """Run notekit with these arguments; returns (exit code, stdout, stderr)."""
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                code = main(list(argv))
            except SystemExit as e:
                code = e.code
        return code, out.getvalue(), err.getvalue()
