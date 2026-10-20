import os
import stat

from checklib import FILES, WORK, Check, stored_name

c = Check()


def mode(rel):
    """The permission bits; None when missing, -1 when the folder above can't be entered."""
    try:
        return stat.S_IMODE(os.lstat(WORK / rel).st_mode)
    except FileNotFoundError:
        return None
    except PermissionError:
        return -1


def missing(rel, m):
    if m is None:
        c.fail(f"{rel} is missing")
    elif m == -1:
        c.fail(f"{rel} can't be reached any more (its folder is closed to its owner too)")
    return m is None or m == -1


for rel in ("bin/deploy.sh", "bin/backup.sh"):
    m = mode(rel)
    if missing(rel, m):
        continue
    c.ok(m & 0o500 == 0o500, f"{rel} is readable and executable by its owner ({m:o})")
    c.ok(m & 0o022 == 0, f"{rel} is not writable by group or others ({m:o})")

for rel in ("secrets/api.key", "secrets/db.env"):
    m = mode(rel)
    if missing(rel, m):
        continue
    c.ok(m & 0o077 == 0, f"{rel} is closed to group and others ({m:o})")
    c.ok(m & 0o400, f"{rel} is still readable by its owner ({m:o})")

m = mode("secrets")
if not missing("the secrets folder", m):
    c.ok(m & 0o077 == 0, f"the secrets folder is closed to group and others ({m:o})")
    c.ok(m & 0o700 == 0o700, f"the owner can still list and open the secrets folder ({m:o})")

# Nothing anywhere became writable by everyone.
loose = []
for dirpath, dirnames, filenames in os.walk(WORK):
    for name in dirnames + filenames:
        p = os.path.join(dirpath, name)
        try:
            if not os.path.islink(p) and os.lstat(p).st_mode & 0o002:
                loose.append(os.path.relpath(p, WORK))
        except OSError:
            pass  # reported above
c.ok(not loose, "nothing is writable by everyone" if not loose else f"writable by everyone: {', '.join(sorted(loose))}")

# Only modes change: every file keeps its content.
changed = []
for dirpath, _dirs, filenames in os.walk(FILES):
    for name in filenames:
        src = os.path.join(dirpath, name)
        rel = os.path.relpath(src, FILES)
        dst = WORK / rel  # no dotfiles in this task
        assert stored_name(rel).as_posix() == rel
        try:
            same = dst.read_bytes() == open(src, "rb").read()
        except OSError:
            same = False
        if not same:
            changed.append(rel)
c.ok(not changed, "every file has its original content" if not changed else f"changed or missing: {', '.join(sorted(changed))}")
c.done()
