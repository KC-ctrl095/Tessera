"""Text matching that ignores case and accents where it can ("koln" finds "Köln")."""

import unicodedata


def fold(text):
    decomposed = unicodedata.normalize("NFKD", text)
    return "".join(ch for ch in decomposed if not unicodedata.combining(ch)).casefold()
