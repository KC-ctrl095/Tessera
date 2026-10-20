"""Parsing the reading list and estimating reading time."""

PAGES_PER_HOUR = 40


def parse_line(line):
    """'Title | Author | pages' -> (title, author, pages)."""
    title, author, pages = (part.strip() for part in line.split("|"))
    return title, author, int(pages)


def reading_time(pages):
    """Hours to read this many pages."""
    return pages / PAGES_PER_HOUR
