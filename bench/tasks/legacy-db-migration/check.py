import ast

from checklib import Check, FILES, WORK, copy_files, diff_lines, original_tree_digest, run, tail, tree_digest, with_hidden

c = Check()
SKIP = {".git", "__pycache__", "node_modules"}
REPO_METHODS = {"all", "one", "one_or_none", "execute", "insert", "scalar"}

leftover = [p.relative_to(WORK).as_posix() for p in WORK.rglob("legacy_db*") if not (set(p.parts) & SKIP)]
c.ok(not leftover, "legacy_db.py is gone" if not leftover else f"still there: {', '.join(leftover)}")

problems = []
for path in sorted(WORK.rglob("*.py")):
    rel = path.relative_to(WORK)
    if set(rel.parts) & SKIP:
        continue
    in_db = rel.parts[0] == "db"
    try:
        tree = ast.parse(path.read_text(encoding="utf-8", errors="replace"))
    except SyntaxError as e:
        problems.append(f"{rel}: doesn't parse ({e.msg}, line {e.lineno})")
        continue
    for node in ast.walk(tree):
        where = f"{rel}:{getattr(node, 'lineno', '?')}"
        if isinstance(node, ast.Import):
            for a in node.names:
                if a.name.split(".")[0] == "legacy_db":
                    problems.append(f"{where}: imports legacy_db")
        elif isinstance(node, ast.ImportFrom):
            if (node.module or "").split(".")[0] == "legacy_db" or any(a.name == "legacy_db" for a in node.names):
                problems.append(f"{where}: imports legacy_db")
            if node.module == "sqlite3" and any(a.name == "connect" for a in node.names) and not in_db:
                problems.append(f"{where}: opens sqlite3 connections outside db/")
        elif isinstance(node, ast.Call):
            if any(k.arg == "fetch" for k in node.keywords):
                problems.append(f"{where}: a call still passes fetch=")
            f = node.func
            if isinstance(f, ast.Attribute) and f.attr == "connect" and isinstance(f.value, ast.Name) and f.value.id == "sqlite3" and not in_db:
                problems.append(f"{where}: opens sqlite3 connections outside db/")
            if isinstance(f, ast.Name) and f.id in ("__import__",) or (isinstance(f, ast.Attribute) and f.attr == "import_module"):
                if any(isinstance(a, ast.Constant) and "legacy_db" in str(a.value) for a in node.args):
                    problems.append(f"{where}: imports legacy_db")
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
            args = node.args
            names = [a.arg for a in args.args + args.kwonlyargs + getattr(args, "posonlyargs", [])]
            if "fetch" in names:
                problems.append(f"{where}: a function still takes fetch= (a legacy_db-style helper)")
            if not in_db and not isinstance(node, ast.Lambda):
                calls = [sub for sub in ast.walk(node) if isinstance(sub, ast.Call) and isinstance(sub.func, ast.Attribute)
                         and sub.func.attr in REPO_METHODS]
                star = args.vararg.arg if args.vararg is not None else None
                if star and any(any(isinstance(a, ast.Name) and a.id == star for a in sub.args) for sub in calls):
                    problems.append(f"{where}: {node.name}(*{star}) wraps Repo like legacy_db.run")
                modes = [d for d in args.defaults + [d for d in args.kw_defaults if d is not None]
                         if isinstance(d, ast.Constant) and d.value in ("all", "one", "id")]
                if calls and modes:
                    problems.append(f"{where}: {node.name}() picks a fetch mode like legacy_db.run")
        elif isinstance(node, ast.Attribute) and node.attr == "_conn" and not in_db:
            problems.append(f"{where}: uses Repo's private connection")
c.ok(not problems, "no legacy_db-style code left" if not problems else "legacy_db-style code left:\n  " + "\n  ".join(problems[:20]))

c.ok(tree_digest(WORK / "db") == original_tree_digest("db"), "db/ is unchanged")
# Adding tests is fine; changing or removing the existing ones isn't (the
# scored run below uses the original tests either way).
weakened = []
for p in sorted((FILES / "tests").rglob("*")):
    if p.is_file():
        rel = p.relative_to(FILES).as_posix()
        if not (WORK / rel).is_file() or diff_lines(rel)[0]:
            weakened.append(rel)
c.ok(not weakened, "the existing tests are intact" if not weakened else f"existing tests were changed or removed: {', '.join(weakened)}")

with with_hidden() as d:
    keep = {p.relative_to(FILES).as_posix() for p in (FILES / "tests").rglob("*") if p.is_file()}
    keep.add("tests/test_migration_hidden.py")
    for p in list((d / "tests").rglob("*.py")):
        if p.relative_to(d).as_posix() not in keep:
            p.unlink()
    copy_files(FILES / "tests", d / "tests")
    code, out = run("python3 -m unittest discover -s tests -t . -v", cwd=d, timeout=90)
failed = [l for l in out.splitlines() if l.endswith(("... FAIL", "... ERROR"))]
c.ok(code == 0, "the project's tests and the migration tests pass (every module, not-found paths, all-or-nothing writes)"
     if code == 0 else "tests fail:\n  " + "\n  ".join(failed[:25]) + "\n" + tail(out, 2500))
c.done()
