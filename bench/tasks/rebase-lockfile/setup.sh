# History for the rebase task: main refreshes the package index, bumps two pins
# and changes how summaries are rounded and sorted; feature/export (from the
# first commit) adds tabwrite, counts per category with CSV export, and a pin.
# deps.lock is regenerated with tools/lock.py in every commit that changes its
# inputs. Fixed authors and dates: the hashes are the same on every run.
set -e
H="$BENCH_TASK/history"
export GIT_COMMITTER_NAME="Bench User" GIT_COMMITTER_EMAIL="bench@example.invalid"

commit() { # author-name author-email date message
    GIT_AUTHOR_NAME="$1" GIT_AUTHOR_EMAIL="$2" GIT_AUTHOR_DATE="$3" GIT_COMMITTER_DATE="$3" \
        git commit -q -m "$4"
}

git branch feature/export

cp "$H/main1/requirements.in" requirements.in
cp "$H/main1/index.json" tools/index.json
python3 tools/lock.py >/dev/null
git add -A
commit "Ruth Okonkwo" "ruth@example.invalid" "2026-09-03T09:12:00+00:00" "Refresh the package index and bump httpx and rich

The index snapshot is from 2026-09-02. httpx 0.27 fixes the proxy retries
we hit in August; rich 13.7 draws our tables faster."

cp "$H/main2/money.py" "$H/main2/report.py" ledger/
cp "$H/main2/test_report.py" tests/
git add -A
commit "Ruth Okonkwo" "ruth@example.invalid" "2026-09-05T14:40:00+00:00" "Round category totals half to even and sort by total

Matches the bank's statements, and puts the biggest spending first."

git checkout -q feature/export
cp "$H/feature1/requirements.in" requirements.in
python3 tools/lock.py >/dev/null
git add -A
commit "Pim de Graaf" "pim@example.invalid" "2026-09-04T11:03:00+00:00" "Add tabwrite for spreadsheet export"

cp "$H/feature2/report.py" "$H/feature2/export.py" "$H/feature2/cli.py" ledger/
cp "$H/feature2/test_report.py" "$H/feature2/test_export.py" tests/
git add -A
commit "Pim de Graaf" "pim@example.invalid" "2026-09-06T16:25:00+00:00" "Export summaries as CSV (and XLSX through tabwrite)

summarize() now also counts the transactions of each category, and the
CLI gets --csv and --xlsx."

cp "$H/feature3/requirements.in" requirements.in
cp "$H/feature3/README.md" README.md
python3 tools/lock.py >/dev/null
git add -A
commit "Pim de Graaf" "pim@example.invalid" "2026-09-08T10:50:00+00:00" "Keep tabwrite below 2.0

tabwrite 2 dropped the CSV dialect options the export relies on."

git checkout -q main
