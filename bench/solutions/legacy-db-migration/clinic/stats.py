"""Patient statistics."""

import db

from . import timeutil


def age_groups(today):
    """{'0-17': n, '18-39': n, '40-64': n, '65+': n} of active patients with a birth date."""
    groups = {"0-17": 0, "18-39": 0, "40-64": 0, "65+": 0}
    for row in db.repo().all("SELECT birth_date FROM patients WHERE active = 1 AND birth_date IS NOT NULL"):
        age = timeutil.age_on(row.birth_date, today)
        if age < 18:
            groups["0-17"] += 1
        elif age < 40:
            groups["18-39"] += 1
        elif age < 65:
            groups["40-64"] += 1
        else:
            groups["65+"] += 1
    return groups
