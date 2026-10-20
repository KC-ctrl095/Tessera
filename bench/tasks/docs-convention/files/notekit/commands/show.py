"""Print the text of a note.

The tags line is left out.
"""

from notekit.errors import fail
from notekit.notes import NotesDirError, load_notes


def add_arguments(parser):
    parser.add_argument("name", help="the note's file name without .md")


def run(args):
    try:
        notes = load_notes(args.dir)
    except NotesDirError as e:
        return fail(str(e))
    for note in notes:
        if note.name == args.name:
            print(note.body)
            return 0
    return fail(f"no note named {args.name!r}")
