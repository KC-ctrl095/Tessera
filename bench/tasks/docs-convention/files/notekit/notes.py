"""Reading notes from the notes folder."""

from dataclasses import dataclass, field
from pathlib import Path


class NotesDirError(Exception):
    """The notes folder doesn't exist or isn't a folder."""


@dataclass
class Note:
    name: str
    tags: list = field(default_factory=list)  # lower case
    body: str = ""

    @property
    def title(self):
        """The first `# ` heading, or the name when there is none."""
        for line in self.body.splitlines():
            if line.startswith("# "):
                return line[2:].strip()
        return self.name

    def has_tag(self, tag):
        return tag.strip().lower() in self.tags


def parse(name, text):
    lines = text.splitlines()
    tags = []
    if lines and lines[0].strip().lower().startswith("tags:"):
        tags = [t.strip().lower() for t in lines[0].strip()[5:].split(",") if t.strip()]
        lines = lines[1:]
    return Note(name, tags, "\n".join(lines).strip("\n"))


def load_notes(folder):
    """Every note (a .md file directly in folder), sorted by name."""
    folder = Path(folder)
    if not folder.is_dir():
        raise NotesDirError(f"notes folder not found: {folder}")
    return [parse(p.stem, p.read_text(encoding="utf-8")) for p in sorted(folder.glob("*.md")) if p.is_file()]
