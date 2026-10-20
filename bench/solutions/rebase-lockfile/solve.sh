# Rebase, stopping at each conflict: requirements.in gets main's pins plus the
# feature's tabwrite line, deps.lock is regenerated (in every commit that
# conflicts, since CI checks each commit), and summarize() keeps main's
# rounding and sort order together with the feature's counts.
set -e
S="$BENCH_SOLUTION"
git checkout -q feature/export
if git rebase main >/dev/null 2>&1; then
    echo "expected conflicts" >&2
    exit 1
fi
n=0
while [ -d .git/rebase-merge ] && [ $n -lt 10 ]; do
    n=$((n + 1))
    case "$(git log -1 --format=%s REBASE_HEAD)" in
        "Add tabwrite"* | "Keep tabwrite"*)
            git show HEAD:requirements.in | grep -v '^tabwrite' > requirements.in
            git show REBASE_HEAD:requirements.in | grep '^tabwrite' >> requirements.in
            python3 tools/lock.py >/dev/null
            git add requirements.in deps.lock
            ;;
        "Export summaries"*)
            cp "$S/report.py" ledger/report.py
            cp "$S/test_report.py" tests/test_report.py
            git add ledger/report.py tests/test_report.py
            ;;
    esac
    GIT_EDITOR=true git rebase --continue >/dev/null 2>&1 || true
done
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -t . 2>&1 | tail -1
python3 tools/lock.py --check
