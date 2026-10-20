# History for the merge task: main moves on (timeout 30 s, duplicate tickets
# dropped) while feature/retries adds retries. Every commit has a fixed
# author, committer and date, so the hashes are the same on every run.
set -e
H="$BENCH_TASK/history"
export GIT_COMMITTER_NAME="Bench User" GIT_COMMITTER_EMAIL="bench@example.invalid"

commit() { # author-name author-email date message
    GIT_AUTHOR_NAME="$1" GIT_AUTHOR_EMAIL="$2" GIT_AUTHOR_DATE="$3" GIT_COMMITTER_DATE="$3" \
        git commit -q -m "$4"
}

git branch feature/retries

cp "$H/main1/config.py" "$H/main1/CHANGELOG.md" .
git add -A
commit "Bench User" "bench@example.invalid" "2026-09-02T10:05:00+00:00" "Raise the request timeout to 30 seconds"

cp "$H/main2/sync.py" "$H/main2/test_dedupe.py" .
git add -A
commit "Bench User" "bench@example.invalid" "2026-09-03T08:40:00+00:00" "Keep one copy of tickets that move between pages during a sync"

git checkout -q feature/retries
cp "$H/feature/config.py" "$H/feature/sync.py" "$H/feature/test_sync.py" "$H/feature/CHANGELOG.md" .
git add -A
commit "Dana Okafor" "dana@example.invalid" "2026-09-04T15:20:00+00:00" "Retry failed requests with exponential backoff"

git checkout -q main
