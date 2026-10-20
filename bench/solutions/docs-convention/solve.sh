set -e
cat > notekit/commands/count.py <<'PY'
"""Count the notes, optionally only those with a tag.

Prints the number of notes in the notes folder.
"""

from notekit.errors import fail
from notekit.notes import NotesDirError, load_notes


def add_arguments(parser):
    parser.add_argument("--tag", help="only count notes with this tag")


def run(args):
    try:
        notes = load_notes(args.dir)
    except NotesDirError as e:
        return fail(str(e))
    if args.tag:
        notes = [note for note in notes if note.has_tag(args.tag)]
    print(len(notes))
    return 0
PY
cat > notekit/commands/__init__.py <<'PY'
"""The commands notekit knows, by name (see CONTRIBUTING.md)."""

from notekit.commands import count
from notekit.commands import list as list_command
from notekit.commands import show

COMMANDS = {
    "count": count,
    "list": list_command,
    "show": show,
}
PY
cat > tests/test_count.py <<'PY'
from tests.helpers import NotesTestCase

NOTES = {
    "standups": "tags: work, ideas\n# Better standups",
    "groceries": "# Groceries",
    "bikes": "tags: Ideas\n# Cargo bike",
}


class CountTest(NotesTestCase):
    def test_counts_every_note(self):
        code, out, err = self.run_cli("--dir", self.make_notes(NOTES), "count")
        self.assertEqual((code, out), (0, "3\n"))

    def test_counts_notes_with_a_tag(self):
        code, out, err = self.run_cli("--dir", self.make_notes(NOTES), "count", "--tag", "IDEAS")
        self.assertEqual((code, out), (0, "2\n"))

    def test_empty_folder(self):
        code, out, err = self.run_cli("--dir", self.make_notes({}), "count")
        self.assertEqual(out, "0\n")

    def test_missing_folder(self):
        code, out, err = self.run_cli("--dir", "/nonexistent/notes", "count")
        self.assertEqual(code, 2)
        self.assertTrue(err.startswith("notekit: error: "))
PY
python3 - <<'PY'
p = "CHANGELOG.md"
s = open(p).read()
s = s.replace("## Unreleased\n\n", "## Unreleased\n\n- Added `count` command: prints how many notes there are, or with `--tag` how many have that tag.\n", 1)
open(p, "w").write(s)
PY
