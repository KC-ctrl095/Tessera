"""Find duplicate customer records before the CRM import."""

from vendor.textnorm import normalize_email, normalize_name


def make_key(record, key_fields, normalize):
    """The record's key: a tuple with one value per key field, or None.

    With normalize=True, "email" values go through normalize_email, "name"
    values through normalize_name, and other values are stripped of
    surrounding whitespace. Values that aren't strings are compared as their
    str(). A field that is missing, None, or empty (after normalizing) makes
    the key None: such a record can't be matched and is never in a group.
    """
    parts = []
    for field in key_fields:
        value = record.get(field)
        if value is None:
            return None
        value = str(value)
        if normalize:
            if field == "email":
                value = normalize_email(value)
            elif field == "name":
                value = normalize_name(value)
            else:
                value = value.strip()
        if value == "":
            return None
        parts.append(value)
    return tuple(parts)


def find_duplicates(records, key_fields=("email",), normalize=True):
    """Groups of records that share a key (see make_key).

    Returns a list of groups. A group is a list of the records (the same
    objects, not copies) sharing one key, in input order. Only keys shared by
    two or more records make a group. Groups are ordered by where their first
    record appears in the input.
    """
    groups = []
    for i, record in enumerate(records):
        key = make_key(record, key_fields, normalize)
        if key is None or any(key == k for k, _ in groups):
            continue
        members = [record]
        for other in records[i + 1:]:
            if make_key(other, key_fields, normalize) == key:
                members.append(other)
        if len(members) > 1:
            groups.append((key, members))
    return [members for _, members in groups]
