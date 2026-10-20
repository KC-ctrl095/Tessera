set -e
python3 - <<'PY'
import pathlib, re
for p in list(pathlib.Path(".").rglob("*.py")) + list(pathlib.Path(".").rglob("*.md")):
    if ".git" in p.parts:
        continue
    s = p.read_text()
    s2 = re.sub(r"\bget_usr\b", "get_user", s)
    s2 = re.sub(r"\bUsrRecord\b", "UserRecord", s2)
    if s2 != s:
        p.write_text(s2)
init = pathlib.Path("shopkit/__init__.py")
s = init.read_text()
s = s.replace('from .reports import spending_report\n', '''from .reports import spending_report

import warnings as _warnings

# Old names, kept for other teams' scripts. Use get_user and UserRecord.
UsrRecord = UserRecord


def get_usr(user_id: int) -> UserRecord:
    """Deprecated: use get_user."""
    _warnings.warn("shopkit.get_usr is deprecated; use shopkit.get_user", DeprecationWarning, stacklevel=2)
    return get_user(user_id)

''')
s = s.replace('__all__ = ["UserRecord", "get_user",', '__all__ = ["UserRecord", "get_user", "UsrRecord", "get_usr",')
init.write_text(s)
PY
cat >> tests/test_users.py <<'PY'


class AliasTest(unittest.TestCase):
    def test_old_names_still_work(self):
        import shopkit
        import warnings

        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            u = shopkit.get_usr(1)
        self.assertIsInstance(u, shopkit.UsrRecord)
PY
