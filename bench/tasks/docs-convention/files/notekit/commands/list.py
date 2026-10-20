"""List notes with their titles.

Prints one line per note: its name, a tab, and its title. With --tag, only the
notes that have that tag.
"""

from notekit.errors import fail
from notekit.notes import NotesDirError, load_notes


def add_arguments(parser):
    parser.add_argument("--tag", help="only notes with this tag")


def run(args):
    try:
        notes = load_notes(args.dir)
    except NotesDirError as e:
        return fail(str(e))
    for note in notes:
        if args.tag and not note.has_tag(args.tag):
            continue
        print(f"{note.name}\t{note.title}")
    return 0
