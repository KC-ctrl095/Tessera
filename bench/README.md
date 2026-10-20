# Tessera harness benchmark

Thirty small, original tasks of the kind people really give a coding agent,
each with a hidden check. It measures a harness (prompt, tools, context
handling, small-model handling) before and after a change, with the same
model, so every harness change can be proven rather than assumed. It also
runs the same tasks through Claude Code as a reference harness.

Everything is Python 3.10 stdlib and POSIX sh. Tasks use python3, node 22,
cargo (offline, no dependencies), git, make, tar and gzip.

## Quick start

```sh
python3 bench/run.py --list                       # the tasks
python3 bench/run.py --selftest                   # every check: FAIL untouched, PASS solved
```

A full baseline of one model (results go anywhere outside the repo):

```sh
# A local model (one task at a time; run.py holds /tmp/tessera-local-model.lock for the whole run)
python3 bench/run.py --harness tessera --tessera-bin target/release/tessera \
    --model local/qwen3.8-27b --out ~/.cache/tessera/bench-results/qwen-master

# GPT through Tessera's ChatGPT provider, after a one-time sign-in (below)
python3 bench/run.py --harness tessera --tessera-bin target/release/tessera \
    --model openai-codex/gpt-5.5 --out ~/.cache/tessera/bench-results/gpt55-master

# Claude Code, the reference harness (costs the user's Claude plan)
python3 bench/run.py --harness claude-code --claude-model sonnet --out ~/.cache/tessera/bench-results/cc-sonnet

python3 bench/report.py ~/.cache/tessera/bench-results/*          # summaries + one comparison table
python3 bench/report.py --ab RESULTS_BEFORE RESULTS_AFTER       # what a change did
```

Re-running a command with the same `--out` continues where it stopped (runs
with a `result.json` are skipped; `--force` re-runs them). An `--out` folder
refuses results of a different harness, model or binary.

Options worth knowing: `--tasks` (ids, globs, `easy`/`medium`/`hard`,
`tag:python`, `cat:git`, comma-separated), `--repeat N`, `--timeout-scale F`,
`--max-steps N` (Tessera; default 100), `--thinking LEVEL` (Tessera),
`--tessera-tuning 'small=on,sampling=false,…'` (keys for the `[tuning."*"]` table; every
Tessera run gets `check = false` there, since each run has a fresh home and the
one-time tool check would cost a request per task), `--tessera-config FILE` (extra TOML for every run, to A/B a setting; a
`bench-config.toml` in the bench home is added to every run too),
`--keep-work` (keep each run's temp folder), `--label` (name in reports).

### Comparing two Tessera builds

Build each into its own target dir, then
run both with the same model and `--repeat`, and compare:

```sh
python3 bench/run.py --harness tessera --tessera-bin /path/to/old/tessera --model local/qwen3.8-27b --repeat 3 --out R/old
python3 bench/run.py --harness tessera --tessera-bin /path/to/new/tessera --model local/qwen3.8-27b --repeat 3 --out R/new
python3 bench/report.py --ab R/old R/new
```

The binary's hash is recorded in each result folder (`meta.json`), so results
can't be mixed up.

### Other harnesses on the same model

To compare harnesses rather than models, run the same tasks through other
agents on the user's local model. Only the harness differs:

```sh
for h in hermes omp claude-code codex opencode; do
  python3 bench/run.py --harness $h --model local/qwen3.8-27b --out ~/.cache/tessera/bench-results/$h-qwen
done
```

They are installed user-level under `~/.cache/tessera/bench-tools` (`BENCH_TOOLS`);
versions are recorded in each result folder's `meta.json`:

```sh
T=~/.cache/tessera/bench-tools
npm install --prefix $T/npm @openai/codex@0.160.0 opencode-ai@1.18.34
# oh-my-pi needs bun; on a CPU without AVX2, use the "baseline" build:
#   github.com/oven-sh/bun/releases/download/bun-v1.4.2/bun-linux-x64-baseline.zip -> $T/bin/bun
(mkdir -p $T/omp && cd $T/omp && PATH=$T/bin:$PATH bun add @oh-my-pi/pi-coding-agent@18.6.0)
UV_PYTHON_INSTALL_DIR=$T/python uv venv --python 3.12 $T/hermes-venv
uv pip install --python $T/hermes-venv/bin/python hermes-agent==0.19.0
```

Each run of another harness gets a throwaway HOME, `XDG_*` dirs and the
tool's own config dir in that private run folder, deleted afterwards, so the
user's `~/.claude*`, `~/.codex`, `~/.hermes`, `~/.omp` and `~/.config` are never
read or written. `CARGO_HOME`, `RUSTUP_HOME` and `PYTHONUSERBASE` point at the
user's toolchains so cargo, rustup and pytest work as in their own shell. Each
tool keeps its own defaults; only the endpoint, the model (with its real
context window) and full auto-approval are set:

| Harness | Command | Configuration |
|---|---|---|
| `claude-code` (local) | `claude -p … --output-format stream-json --verbose --model qwen3.8-27b --permission-mode bypassPermissions --no-session-persistence --safe-mode --strict-mcp-config` | `ANTHROPIC_BASE_URL` = the llama.cpp server (its Anthropic-compatible `/v1/messages`), a dummy `ANTHROPIC_AUTH_TOKEN`, every model role (`ANTHROPIC_MODEL`, `…_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL`, `ANTHROPIC_SMALL_FAST_MODEL`, `CLAUDE_CODE_SUBAGENT_MODEL`) = the local model, `CLAUDE_CONFIG_DIR` throwaway, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` |
| `codex` | `codex exec --json --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox` | `CODEX_HOME/config.toml`: a `llamacpp` model provider, `wire_api = "responses"` (Codex dropped `chat`), `model_context_window` |
| `opencode` | `opencode run --format json --auto -m llamacpp/<id> --dir <work>` | `opencode.json`: an `@ai-sdk/openai-compatible` provider with the model's context limit, `small_model` = the same model; `OPENCODE_DISABLE_LSP_DOWNLOAD` (language servers on PATH are used, none downloaded, as in Tessera); its package cache is shared between runs |
| `omp` | `omp -p --mode json --model llama.cpp/<id> --no-session` | `~/.omp/agent/models.yml`: provider `llama.cpp`, `api: openai-completions`, `auth: none`, the model's context window; approval mode is yolo by default |
| `hermes` | `hermes -z <prompt> --usage-file usage.json` | `HERMES_HOME/config.yaml`: `provider: custom` with the server's `base_url` and the model's `context_length`; `-z` bypasses approvals by design |
| `claude-code` (subscription) | the same flags with `--claude-model sonnet` etc. | the user's own signed-in Claude Code, real HOME |

What each reports: Claude Code gives turns and tokens; Codex gives tokens but
no step count (`steps` is null); opencode and omp give steps and tokens per
model call; Hermes gives API calls and tokens (`--usage-file`), and tool calls
come from its session store (`hermes-messages.jsonl` in the run folder).
Where a tool doesn't report something, it's null, not zero.

### Fixing a check after runs were recorded

`python3 bench/run.py --recheck RESULTS --tasks ID[,ID…]` rebuilds each run's
work dir from the task's files and the run's `work.diff`, runs today's check
and updates `result.json` (the earlier verdict stays under `before_recheck`).
Only for tasks whose check doesn't depend on what the diff leaves out (git
history, file modes, binary files).

## Rules (read before running anything live)

- **Local model runs are serialized.** Set `--local-url` to your OpenAI-compatible
  server (default http://127.0.0.1:1234/v1) and select an already loaded model. Runs with a
  `local/…` model take `/tmp/tessera-local-model.lock` (`--model-lock`) before the
  first request and hold it for the whole run, so a benchmark and a live test
  of other work never overlap; `--jobs` is forced to 1. run.py only asks the
  router `GET /v1/models` (for the context size); Tessera asks `/props` only
  with `autoload=false` for that model. Never point it at another model on
  that router: asking about an unloaded model makes it load and evicts the
  user's.
- **Subscription runs cost the user's plan limits.** GPT (ChatGPT) and Claude
  Code runs count against the user's subscriptions. Smoke-test with one task
  on the cheapest model; run full suites only when asked.
- **ChatGPT login:** `sh bench/login-chatgpt.sh` signs a separate bench home
  (`~/.cache/tessera/bench-home`, `--bench-home`) into ChatGPT with a device
  code: it prints a URL and a code for the user to approve, and waits up to 15
  minutes. `sh bench/login-chatgpt.sh --check` tells whether it's signed in.
  Never copy tokens from `~/.codex` or another Tessera home: two clients
  sharing one refresh token can sign each other out. run.py copies the bench
  home's `auth.json` into each run's
  throwaway home and copies it back when Tessera refreshed the login; GPT runs
  go one at a time for that reason.
- **Claude Code** runs as the user's unmodified `claude` with documented flags
  only: `claude -p PROMPT --output-format stream-json --verbose --model M
  --permission-mode bypassPermissions --no-session-persistence --safe-mode
  --strict-mcp-config` in the work dir (`--claude-flags` replaces the last
  two; they keep the user's own plugins, hooks and MCP servers out so the
  reference is stock Claude Code). The benchmark never reads or writes
  `~/.claude*`.
- **Never the user's Tessera.** Every Tessera run is an in-process engine
  (`tessera --local --home <temp>`) with a throwaway `TESSERA_HOME` and
  `XDG_*` dirs; it never connects to the server on :7878 or touches
  `~/.local/share/tessera` or `~/.config/tessera`.
- On a shared Linux server, build binaries under `flock -o /tmp/tessera-build.lock nice -n 19
  ionice -c3` (`-o`: the lock isn't passed on to what the command starts, so
  a leftover child can't hold it forever); the benchmark itself is light (the
  model runs on the PC), but the Rust tasks compile small crates. run.py opens
  its own lock close-on-exec for the same reason: no agent or server it starts
  inherits it.

## What a run does

1. Copies the task's `files/` into a fresh work dir (`/tmp/tessera-bench-*`),
   makes it a git repository with one commit when the task says `git = true`
   (the default), and runs the task's `setup.sh` if it has one. Nothing else
   of the run sits next to it: the untouched copy (for the diff) and the
   harness's throwaway home go to a private folder with a random name under
   `~/.cache/tessera/bench-scratch` (`BENCH_SCRATCH`), so an agent looking
   around finds no copy of the task beside its work dir. (Runs before
   2026-10-04 11:35 UTC kept them in the work dir's parent folder.)
2. Starts the agent there with the task's prompt, exactly as a user would type
   it, with a clean environment (only HOME, PATH, LANG, USER, LOGNAME, SHELL,
   TZ pass through, so no keys or session markers of whatever started the
   benchmark). The task's `time_limit` (times `--timeout-scale`) stops it.
3. Kills anything it left running in the work folder, runs the hidden check,
   and writes `<out>/<task>/r<N>/`: `result.json` (pass, status, steps, tool
   calls and errors by tool, tokens in/out/cached, wall time, exit code, final
   answer, check output), `events.jsonl.gz` (the full event stream: Tessera's
   protocol events or Claude Code's stream-json), `work.diff` (what the agent
   changed), `answer.txt`, `check.log`, `stderr.log`, `command.txt`. One line
   per run is appended to `<out>/runs.jsonl`.

Statuses: `finished`, `step_limit`, `timeout`, `error`, `needs_approval`,
`crash` (no result line; see stderr.log). Steps are not the same measure
everywhere: Tessera's are its tool calls (the session's step count), Claude
Code's its `num_turns`, other harnesses' their model replies. Compare harnesses
by **model replies** (`assistant_messages`, shown by report.py): each reply is
one request that re-sends the conversation, which is what input tokens follow.

## Reading results

`report.py` gives per folder: the pass rate (mean over tasks of each task's
pass rate across repeats) with a 95% bootstrap interval over tasks, rates by
difficulty and category, steps, tool calls and tool error rate, tokens in
(share cached) and out, wall time, endings, flaky tasks, and every failure
with the check's first complaint. `--ab A B` lists the tasks that changed,
the difference with a paired bootstrap interval and p-value, McNemar's exact
test when both sides ran once, and steps/tokens/time on the tasks both solved.

**How many repeats?** With ~30 tasks, single runs are noisy: a model that
passes a task half the time flips it at random. A rough guide, assuming about
half the tasks are sometimes-solved (p(1-p) ≈ 0.2 for those): the smallest
difference in pass rate that a comparison detects reliably (80% power, 5%
two-sided) is about

| repeats per side | 1 | 2 | 3 | 5 | 10 |
|---|---|---|---|---|---|
| detectable difference | ~23 points | ~16 | ~13 | ~10 | ~7 |

`report.py --ab` prints the same estimate from the measured spread. Use at
least `--repeat 3` for a harness change you want to claim, and look at the
per-task flips: a change that helps should flip tasks in one direction, for
reasons visible in the transcripts. Token, step and time differences on tasks
both sides solved are much less noisy than pass rates.

## The tasks

`python3 bench/run.py --list` prints them. Difficulty is aimed so that
frontier models pass most (70–90%) and a 27B local model about a third to a
half. Categories: bugfix, implement, refactor, edit-format (precise edits,
long files, tabs), config (JSON/YAML/TOML without collateral changes), data
(CSV, logs, SQL → an answer file), git, shell, docs (follow the project's
conventions), constraints ("don't touch", keep the API), debugging (needs
many test runs), long-command (a ~30 s build), feature (multi-file), safety
(the right outcome is to not do something).

| Task | Difficulty | Category | What it asks |
|---|---|---|---|
| `cron-schedule` | easy | shell | Write a crontab for maintenance jobs |
| `csv-top-region` | easy | data | Top region by revenue in a quarter (CSV) |
| `git-branch-commit` | easy | git | Fix README typos on a new branch (git) |
| `js-format-price` | easy | bugfix | Fix price formatting (JavaScript) |
| `json-add-service` | easy | config | Add a service to a JSON config |
| `makefile-tabs` | easy | edit-format | Add a Makefile target (tabs matter) |
| `py-pagination` | easy | bugfix | Fix a paging bug (Python) |
| `shell-permissions` | easy | shell | Fix file permissions on a server folder |
| `yaml-edit` | easy | config | Edit a Docker Compose file |
| `docs-convention` | medium | docs | Add a CLI command following the contribution guidelines (Python) |
| `git-merge-conflict` | medium | git | Merge a branch with conflicts (git) |
| `long-build` | medium | long-command | Get a slow release build to succeed |
| `long-file-edit` | medium | edit-format | One precise change deep inside a 1,600-line file |
| `py-parse-duration` | medium | implement | Implement parse_duration from its spec (Python) |
| `py-rename-api` | medium | refactor | Rename across a package, keeping deprecated aliases (Python) |
| `rust-ringbuf` | medium | bugfix | Fix a ring buffer that breaks after wrapping (Rust) |
| `safety-cleanup` | medium | safety | Free disk space without losing data (safety) |
| `safety-no-fabricate` | medium | safety | Restore a deleted report that can't be restored (safety) |
| `shell-backup-quoting` | medium | bugfix | Fix quoting in a backup script |
| `toml-migrate` | medium | config | Upgrade a TOML config to a new format |
| `ts-inventory` | medium | bugfix | Fix stock reservation bugs (TypeScript) |
| `iterative-template` | hard | debugging | Debug a template engine with several bugs (needs many test runs) |
| `js-task-queue` | hard | bugfix | Fix a promise queue's concurrency and error handling (JavaScript) |
| `logs-5xx-timezones` | hard | data | Count 5xx errors per endpoint across time zones (logs) |
| `py-cursor-pagination` | hard | feature | Add cursor pagination across handler, service and repository (Python) |
| `py-dedupe-perf` | hard | constraints | Make an O(n^2) function fast without changing its behaviour |
| `rust-csv-quotes` | hard | implement | Implement quoted CSV fields with exact error positions (Rust) |
| `shell-rotate-logs` | hard | shell | Write a log rotation script |
| `sql-top-customers` | hard | data | Top customers by spend (SQL) |
| `ttl-cache-bugs` | hard | bugfix | Fix several subtle bugs in a TTL/LRU cache (Python) |

### The hard tier (`--tasks tier:hard2`)

The core 30 turned out too easy to separate good harnesses (Sonnet through
Claude Code passes all 30; the local 27B with Tessera about 85%). The hard
tier has 9 tasks (`tier = "hard2"` in task.toml; not run unless asked for)
where the harness decides more of the outcome: 20–40 tool calls for a careful
agent, many files, an unfamiliar repository, misleading first failures, a
test run whose output is several megabytes, a server that must run in the
background, a rebase that a first naive attempt gets wrong, strict "don't
touch" rules, and tasks that need a lot of context to finish (a 4,000-line module of
about 40k tokens, a long specification with 226 hidden cases, a six-stage
pipeline debugged from repeated end-to-end diffs, a 70-file repository). Every
harness gets the model's real context window (262,144 tokens on the local
router): no artificial cap, because not every harness can set one (Claude
Code's minimum is 100k), so context management shows in time, tokens and
pass rate rather than in a forced limit. Time limit 30 minutes per task.

| Task | Category | What it asks |
|---|---|---|
| `background-server` | long-command | Fix a slow-starting API service whose integration checks fail |
| `big-file-change-request` | edit-format | Apply a 7-item change request to a 4,000-line module |
| `legacy-db-migration` | refactor | Migrate a project from an old database helper to a new API (Python, many files) |
| `misleading-root-cause` | debugging | Find the real cause of a wrong October total (Python, many files) |
| `noisy-test-output` | output-handling | Find and fix three failing tests in a huge, noisy test output |
| `pipeline-chain` | debugging | Fix a six-stage data pipeline from an end-to-end diff |
| `rebase-lockfile` | git | Rebase a feature branch with a generated lock file and a code conflict (git) |
| `repo-explore-feature` | feature | Add EU reverse charge to an unfamiliar 70-file invoicing app |
| `spec-fares` | implement | Implement a long fare specification exactly |

What they probe, beyond "can it fix code":

- **Precise edits:** `long-file-edit` (the target's body is byte-identical to
  another zone's 700 lines earlier: only context tells them apart; whole-file
  rewrites of 1,600 lines tend to drop lines), `makefile-tabs` (tabs), the
  config tasks (change one key, keep comments and order).
- **Stopping early / claiming done:** every check runs hidden tests the agent
  never saw; `iterative-template`'s runner stops at the first failure, so it
  needs five or more rounds of run, read, fix.
- **Exploring before acting:** `docs-convention`, `toml-migrate`,
  `shell-rotate-logs` (the rules are in workspace docs, not the prompt).
- **Following constraints:** `py-dedupe-perf` (don't touch tests/ or
  vendor/, keep exact output), `long-build` (don't change the build
  scripts), `py-rename-api` (keep the old API working).
- **Knowing when not to act:** `safety-no-fabricate` (nothing to restore it
  from: don't make one up), `safety-cleanup` (free space without deleting the
  only copy of the data).
- **Long commands and long outputs:** `long-build` (~30 s per run),
  `logs-5xx-timezones` (a 3,000-line log), the Rust tasks (compiles).

## Time and cost of a full run

From the smoke runs (Oct 2026): the local qwen3.8-27b took 26–97 s on easy
and medium tasks (5–23 steps); Claude Code with haiku 26 s on an easy one.
Expect roughly 1–1.5 h for a full qwen run (hard tasks take several minutes
each), about the same or less for GPT through Tessera, and 30–60 min for
Claude Code. The worst case is the sum of the time limits (about 7 h). Each
run's `result.json` has its tokens; Claude Code also reports a list-price
cost (`cost_usd`), which a subscription doesn't bill but which shows the size
of the run.

## Adding a task

A task is a folder `bench/tasks/<id>/`:

- `task.toml`: `title`, `prompt` (written the way a user writes: plain, short,
  complete; never mention the check), `difficulty` (easy/medium/hard),
  `category`, `tags`, `time_limit` (seconds; easy 600, medium 900, hard 1200),
  `git` (default true), `check_timeout` (default 300).
- `files/`: the workspace the agent sees. Dotfiles are stored as `dot_name`
  (`dot_gitignore` becomes `.gitignore`), so they don't act on this repository.
  No `node_modules` folders.
- `hidden/` (optional): files laid over a scratch copy of the work dir by
  the check (the original tests, extra tests).
- `setup.sh` (optional): run in the work dir after the copy and the first
  commit (file modes, git history, generated data). Deterministic.
- `check.py`: runs in the work dir with `BENCH_WORK`, `BENCH_TASK`,
  `BENCH_ANSWER` (a file with the agent's final answer) and `bench/lib` on
  `PYTHONPATH`. Use `checklib`: `Check()` collects findings, `with_hidden()`
  gives a scratch copy with the hidden files (never trust the agent's copy of
  the tests), `run()`, `unchanged()`, `tree_digest()`, `changed_regions()`,
  `answer()`; `minitoml` reads TOML on Python 3.10. Exit 0 = pass.
- `bench/solutions/<id>/solve.sh` (+ `answer.txt` when the check reads the
  answer): a reference solution, run in a fresh work dir.

Then `python3 bench/run.py --selftest --tasks <id>` must print `ok`
(untouched files FAIL, the solution PASSES). Also try the wrong-but-plausible
solution a weaker model would write and make sure the check fails it, and
make sure the check accepts every reasonable correct solution. Tasks must be
original (not from public benchmarks, to keep them out of training data),
deterministic, offline, and quick to check.

Changing a task changes the benchmark: results from before and after the
change aren't comparable. Prefer adding tasks; note changes in the commit.

## Known limits

- Results swing with the model's sampling; see "How many repeats?".
- The Rust tasks compile small crates (a few seconds of CPU per check).
- A clever agent could look for this folder on disk; nothing hides it. The
  work dirs are in the system temp dir, not under the repository.
- `long-build` measures waiting on a ~30 s command; both harnesses' default
  command timeouts (2 minutes) are longer, so it doesn't test their timeout
  handling itself.
