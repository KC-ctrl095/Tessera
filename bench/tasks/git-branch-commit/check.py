import subprocess

from checklib import FILES, WORK, Check, _undot, git, original_bytes, original_text


def show(spec, as_text=True):
    """A file at a revision (`rev:path`), as git stores it."""
    return subprocess.run(["git", "show", spec], cwd=WORK, capture_output=True, text=as_text).stdout


BRANCH = "fix/readme-typos"

c = Check()

# main must still be the untouched initial commit.
code, count = git("rev-list", "--count", "main")
c.ok(code == 0 and count == "1", "main still has exactly one commit" if count == "1" else f"main has {count} commits (expected the initial commit only)")
code, files_on_main = git("ls-tree", "-r", "--name-only", "main")
handed_out = sorted("/".join(_undot(x) for x in p.relative_to(FILES).parts) for p in FILES.rglob("*") if p.is_file())
same = code == 0 and sorted(files_on_main.splitlines()) == handed_out
for rel in files_on_main.splitlines():
    code2, _ = git("cat-file", "-e", f"main:{rel}")
    if code2 != 0:
        same = False
        continue
    blob = show(f"main:{rel}", as_text=False)
    if blob != original_bytes(rel):
        same = False
c.ok(same, "main's files are as they were" if same else "main's files changed")
main_readme = show("main:README.md")
c.ok(main_readme.count("recieve") == 2, "README.md on main still has its two typos")

code, _ = git("rev-parse", "--verify", "-q", f"refs/heads/{BRANCH}")
if not c.ok(code == 0, f"branch {BRANCH} exists" if code == 0 else f"no branch named {BRANCH}"):
    c.done()

code, main_sha = git("rev-parse", "main")
code, parents = git("rev-list", "--parents", "-n", "1", BRANCH)
parent_list = parents.split()[1:]
c.ok(parent_list == [main_sha], f"{BRANCH} is one commit on top of main" if parent_list == [main_sha] else f"{BRANCH} is not exactly one commit on top of main (parents: {parent_list})")

code, changed = git("diff", "--name-only", "main", BRANCH)
c.ok(changed.splitlines() == ["README.md"], "the commit touches only README.md" if changed.splitlines() == ["README.md"] else f"files changed on the branch: {changed.split() or 'none'}")

new_readme = show(f"{BRANCH}:README.md")
expected = original_text("README.md").replace("recieve", "receive")
c.ok("recieve" not in new_readme.lower(), "no 'recieve' left on the branch" if "recieve" not in new_readme.lower() else "'recieve' is still in README.md on the branch")
c.ok(new_readme.rstrip("\n") == expected.rstrip("\n"), "README.md differs only by the fixed words" if new_readme.rstrip("\n") == expected.rstrip("\n") else "README.md on the branch has other changes besides the two fixes")

# Uncommitted changes to tracked files count as unfinished work. Untracked
# leftovers (an editor backup, say) are not part of what was asked.
code, status = git("status", "--porcelain", "--untracked-files=no")
c.ok(code == 0 and status == "", "no uncommitted changes" if status == "" else "uncommitted changes:\n" + status)
c.done()
