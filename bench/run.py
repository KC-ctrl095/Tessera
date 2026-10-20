#!/usr/bin/env python3
"""Tessera's harness benchmark: run tasks with an agent and check the results.

  bench/run.py --list
  bench/run.py --selftest                       # every check: FAIL untouched, PASS solved
  bench/run.py --harness tessera --tessera-bin target/release/tessera \\
               --model local/qwen3.8-27b --out results/qwen-master
  bench/run.py --harness claude-code --claude-model haiku --tasks py-pagination --out results/cc

Each run gets a fresh temporary work dir (a copy of the task's `files/`, made a
git repository when the task says so), a throwaway Tessera home, and its own
result folder: result.json, the event log, the final answer, the check's
output and a diff of what the agent changed. A run whose result.json exists is
skipped, so an interrupted run continues with the same command.
See bench/README.md.
"""

import argparse
import concurrent.futures
import datetime
import fnmatch
import gzip
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from collections import Counter
from pathlib import Path

sys.dont_write_bytecode = True  # no __pycache__ next to the tasks
BENCH = Path(__file__).resolve().parent
TASKS = BENCH / "tasks"
SOLUTIONS = BENCH / "solutions"
LIB = BENCH / "lib"
sys.path.insert(0, str(LIB))
import minitoml  # noqa: E402
from checklib import copy_files  # noqa: E402

DIFFICULTIES = ("easy", "medium", "hard")
DEFAULT_LOCAL_URL = "http://127.0.0.1:1234/v1"
DEFAULT_BENCH_HOME = Path(os.environ.get("BENCH_HOME", "~/.cache/tessera/bench-home")).expanduser()
# Other harnesses, installed user-level (see README "Other harnesses").
TOOLS = Path(os.environ.get("BENCH_TOOLS", "~/.cache/tessera/bench-tools")).expanduser()
# Everything a run keeps besides its work dir (the untouched copy for the
# diff, harness homes): away from the work dir, so an agent looking around
# /tmp finds no copy of the task next to its own.
SCRATCH = Path(os.environ.get("BENCH_SCRATCH", "~/.cache/tessera/bench-scratch")).expanduser()
REAL_HOME = Path.home()
# Environment passed to agents: only these from ours (no keys, no session
# markers of whatever started the benchmark, no cargo target dirs).
ENV_KEEP = ("HOME", "PATH", "LANG", "LC_ALL", "USER", "LOGNAME", "SHELL", "TZ")
GIT_ID = {
    "GIT_AUTHOR_NAME": "Bench User",
    "GIT_AUTHOR_EMAIL": "bench@example.invalid",
    "GIT_COMMITTER_NAME": "Bench User",
    "GIT_COMMITTER_EMAIL": "bench@example.invalid",
    "GIT_AUTHOR_DATE": "2026-09-01T09:00:00+00:00",
    "GIT_COMMITTER_DATE": "2026-09-01T09:00:00+00:00",
}
print_lock = threading.Lock()


def log(msg):
    with print_lock:
        print(msg, flush=True)


# ---- tasks -----------------------------------------------------------------


class Task:
    def __init__(self, path):
        self.dir = path
        self.id = path.name
        meta = minitoml.load_file(path / "task.toml")
        self.meta = meta
        self.title = meta.get("title", self.id)
        self.prompt = meta["prompt"].strip()
        self.difficulty = meta.get("difficulty", "medium")
        self.category = meta.get("category", "other")
        self.tags = list(meta.get("tags", []))
        self.time_limit = int(meta.get("time_limit", 900))
        self.check_timeout = int(meta.get("check_timeout", 300))
        self.git = bool(meta.get("git", True))
        # "core": the original 30; "hard2": long, many-file, context-heavy tasks.
        self.tier = meta.get("tier", "core")
        self.check = path / "check.py" if (path / "check.py").exists() else path / "check"

    def validate(self):
        problems = []
        if self.difficulty not in DIFFICULTIES:
            problems.append(f"difficulty must be one of {DIFFICULTIES}")
        if not (self.dir / "files").is_dir():
            problems.append("no files/ folder")
        if not self.check.exists():
            problems.append("no check.py")
        if not self.prompt:
            problems.append("empty prompt")
        for p in (self.dir / "files").rglob("*"):
            if p.name in ("__pycache__", ".git", "node_modules", "target") or p.suffix == ".pyc":
                problems.append(f"files/ holds {p.relative_to(self.dir)} (generated; remove it)")
                break
            if p.name.startswith(".") and p.name != ".":
                problems.append(f"files/ holds {p.relative_to(self.dir)}: store dotfiles as dot_{p.name[1:]}")
                break
        return problems


def load_tasks(patterns=None, default_tier="core"):
    """Tasks matching any pattern; without patterns, the default tier (None = all)."""
    tasks = []
    for p in sorted(TASKS.iterdir()):
        if not (p / "task.toml").exists():
            continue
        t = Task(p)
        if patterns:
            if not any(_match(t, pat) for pat in patterns):
                continue
        elif default_tier and t.tier != default_tier:
            continue
        tasks.append(t)
    return tasks


def _match(t, pat):
    if pat == "all":
        return True
    if pat.startswith("tier:"):
        return t.tier == pat[5:]
    if pat.startswith("tag:"):
        return pat[4:] in t.tags
    if pat in DIFFICULTIES:
        return t.difficulty == pat
    if pat.startswith("cat:"):
        return t.category == pat[4:]
    return fnmatch.fnmatch(t.id, pat)


def split_patterns(values):
    out = []
    for v in values or []:
        out.extend(x for x in re.split(r"[,\s]+", v) if x)
    return out


# ---- work dirs ---------------------------------------------------------------


def base_env():
    env = {k: os.environ[k] for k in ENV_KEEP if k in os.environ}
    env.setdefault("LANG", "C.UTF-8")
    env["TERM"] = "dumb"
    return env


def sh(cmd, cwd, env=None, timeout=300):
    p = subprocess.run(
        cmd,
        cwd=str(cwd),
        env=env,
        shell=isinstance(cmd, str),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        timeout=timeout,
    )
    if p.returncode != 0:
        raise RuntimeError(f"`{cmd}` failed in {cwd}:\n{p.stdout}")
    return p.stdout


def new_scratch():
    """A private folder for one run's bookkeeping (0700, random name)."""
    SCRATCH.mkdir(parents=True, exist_ok=True)
    os.chmod(SCRATCH, 0o700)
    return Path(tempfile.mkdtemp(prefix="", dir=SCRATCH))


def prepare(task, work):
    """Copy the task's files into the (empty) work dir, make it a repo, run setup.sh."""
    work = Path(work)
    copy_files(task.dir / "files", work)
    env = dict(base_env(), **GIT_ID, BENCH_TASK=str(task.dir), BENCH_WORK=str(work))
    if task.git:
        sh(["git", "init", "-q", "-b", "main"], work, env)
        sh(["git", "config", "user.name", GIT_ID["GIT_AUTHOR_NAME"]], work, env)
        sh(["git", "config", "user.email", GIT_ID["GIT_AUTHOR_EMAIL"]], work, env)
        sh(["git", "config", "commit.gpgsign", "false"], work, env)
        sh(["git", "add", "-A"], work, env)
        sh(["git", "commit", "-q", "-m", "Initial commit"], work, env)
    setup = task.dir / "setup.sh"
    if setup.exists():
        sh(["sh", str(setup)], work, env)
    return work


def run_check(task, work, answer_text, out_dir=None):
    ans = Path(tempfile.mkstemp(prefix="bench-answer-")[1])
    ans.write_text(answer_text or "", encoding="utf-8")
    env = dict(
        base_env(),
        BENCH_TASK=str(task.dir),
        BENCH_WORK=str(work),
        BENCH_ANSWER=str(ans),
        PYTHONPATH=str(LIB),
        PYTHONDONTWRITEBYTECODE="1",
    )
    cmd = [sys.executable, str(task.check)] if task.check.suffix == ".py" else [str(task.check)]
    t0 = time.time()
    try:
        p = subprocess.run(
            cmd,
            cwd=str(work),
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
            timeout=task.check_timeout,
            start_new_session=True,
        )
        passed, output = p.returncode == 0, p.stdout
    except subprocess.TimeoutExpired as e:
        out = e.stdout.decode("utf-8", "replace") if isinstance(e.stdout, bytes) else (e.stdout or "")
        passed, output = False, out + f"\n[check timed out after {task.check_timeout}s]"
    finally:
        ans.unlink(missing_ok=True)
    if out_dir:
        (out_dir / "check.log").write_text(output, encoding="utf-8")
    return passed, output, time.time() - t0


SNAP_SKIP = {".git", "target", "node_modules", "__pycache__", ".pytest_cache"}


def snapshot(work, dest):
    shutil.copytree(work, dest, symlinks=True, ignore=lambda d, n: [x for x in n if x in SNAP_SKIP])


def work_diff(before, after, limit=400_000):
    """A unified diff of text files (binary and huge files are named only)."""
    import difflib

    def files(root):
        out = {}
        for dp, dns, fns in os.walk(root):
            dns[:] = [d for d in dns if d not in SNAP_SKIP]
            for f in fns:
                p = Path(dp) / f
                out[p.relative_to(root).as_posix()] = p
        return out

    a, b = files(before), files(after)
    chunks = []
    for rel in sorted(set(a) | set(b)):
        pa, pb = a.get(rel), b.get(rel)
        da = pa.read_bytes() if pa and not pa.is_symlink() else b""
        db = pb.read_bytes() if pb and not pb.is_symlink() else b""
        if pa and pb and da == db and (pa.stat().st_mode & 0o111) == (pb.stat().st_mode & 0o111):
            continue
        if pa and pb and da == db:
            chunks.append(f"mode changed: {rel} {oct(pa.stat().st_mode & 0o777)} -> {oct(pb.stat().st_mode & 0o777)}\n")
            continue
        if b"\0" in da[:4096] or b"\0" in db[:4096] or len(da) + len(db) > 2_000_000:
            chunks.append(f"binary or large file {'added' if not pa else 'removed' if not pb else 'changed'}: {rel}\n")
            continue
        ta = da.decode("utf-8", "replace").splitlines(keepends=True)
        tb = db.decode("utf-8", "replace").splitlines(keepends=True)
        chunks.append("".join(difflib.unified_diff(ta, tb, f"a/{rel}" if pa else "/dev/null", f"b/{rel}" if pb else "/dev/null")))
        if sum(map(len, chunks)) > limit:
            chunks.append("\n[diff truncated]\n")
            break
    return "".join(chunks)


def kill_tree(pid):
    """Stop the process group we started, then anything still running in the run's folder."""
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(pid, sig)
        except ProcessLookupError:
            return
        time.sleep(3 if sig == signal.SIGTERM else 0.5)


def make_readable(root):
    """Give the owner access to everything again (an agent may have run
    `chmod -R 600` on a folder), so diffs and cleanup don't fail."""
    for dirpath, dirnames, filenames in os.walk(root, onerror=lambda e: None):
        for name in dirnames + filenames:
            p = os.path.join(dirpath, name)
            try:
                if not os.path.islink(p):
                    mode = os.stat(p).st_mode
                    want = mode | (0o700 if os.path.isdir(p) else 0o600)
                    if want != mode:
                        os.chmod(p, want & 0o7777)
            except OSError:
                pass


def kill_leftovers(*roots):
    """Processes we own whose working directory is inside one of these folders."""
    roots = [str(r) for r in roots if r]
    me = os.getuid()
    killed = 0
    for d in os.listdir("/proc"):
        if not d.isdigit() or int(d) == os.getpid():
            continue
        try:
            if os.stat(f"/proc/{d}").st_uid != me:
                continue
            cwd = os.readlink(f"/proc/{d}/cwd")
        except OSError:
            continue
        if any(cwd == root or cwd.startswith(root + "/") for root in roots):
            try:
                os.kill(int(d), signal.SIGKILL)
                killed += 1
            except OSError:
                pass
    return killed


# ---- harnesses ---------------------------------------------------------------


class Harness:
    name = "?"

    def describe(self):
        return {}

    def label(self):
        return self.name

    def serial(self):
        """True when runs must go one at a time (a single-slot or shared login)."""
        return False


class TesseraHarness(Harness):
    name = "tessera"

    def __init__(self, args):
        self.bin = Path(args.tessera_bin).expanduser().resolve()
        if not self.bin.exists():
            sys.exit(f"no tessera binary at {self.bin}")
        if not args.model or "/" not in args.model:
            sys.exit("--model provider/model is required for --harness tessera")
        self.model = args.model
        self.provider = args.model.split("/", 1)[0]
        self.max_steps = args.max_steps
        self.thinking = args.thinking
        self.local_url = args.local_url
        self.bench_home = Path(args.bench_home).expanduser()
        self.extra_config = Path(args.tessera_config).read_text() if args.tessera_config else ""
        # [tuning."*"] for every run: no one-time tool-use check (each run has
        # a fresh home, so it would cost an extra request per task), plus
        # --tessera-tuning keys for A/B runs.
        self.tuning = {"check": False}
        self.tuning.update(parse_tuning(args.tessera_tuning or ""))
        self.auth_lock = threading.Lock()
        self.version = subprocess.run([str(self.bin), "--version"], capture_output=True, text=True).stdout.strip()
        self.sha = hashlib.sha256(self.bin.read_bytes()).hexdigest()[:16]
        self.base_config = self._base_config()

    def _base_config(self):
        parts = []
        # Not the bench home's own config.toml: Tessera writes that one (the
        # login may set a default model there).
        home_cfg = self.bench_home / "bench-config.toml"
        base = home_cfg.read_text() if home_cfg.exists() else ""
        if not re.search(r"(?m)^default_model\s*=", base + self.extra_config):
            parts.append(f'default_model = "{self.model}"\n')
        parts += [base, self.extra_config]
        joined = "\n".join(parts)
        if self.provider == "local" and not re.search(r'(?m)^id\s*=\s*"local"', joined):
            model_id = self.model.split("/", 1)[1]
            ctx = local_context(self.local_url, model_id)
            joined += (
                "\n[[provider]]\nid = \"local\"\nkind = \"openai_chat\"\n"
                f"base_url = \"{self.local_url}\"\ndiscover = false\n"
                f"[[provider.models]]\nid = \"{model_id}\"\ncontext_window = {ctx}\nreasoning = true\n"
            )
        if re.search(r'(?m)^\[tuning\."\*"\]', joined):
            sys.exit('the extra config already has [tuning."*"]; put those keys in --tessera-tuning instead')
        joined += '\n[tuning."*"]\n' + "".join(f"{k} = {toml_value(v)}\n" for k, v in self.tuning.items())
        minitoml.loads(joined)  # fail early on a broken config
        return joined

    def serial(self):
        # The local server has one slot; a ChatGPT login has one refresh token.
        return self.provider in ("local", "openai-codex")

    def describe(self):
        return {
            "harness": "tessera",
            "model": self.model,
            "tessera_bin": str(self.bin),
            "tessera_version": self.version,
            "tessera_sha256": self.sha,
            "max_steps": self.max_steps,
            "thinking": self.thinking,
            "tuning": self.tuning,
            "config": self.base_config,
        }

    def label(self):
        return f"tessera {self.model}"

    def run(self, task, work, root, out_dir, time_limit):
        home = root / "h"
        home.mkdir()
        (home / "config.toml").write_text(self.base_config)
        auth_src = self.bench_home / "auth.json"
        if auth_src.exists():
            with self.auth_lock:
                shutil.copy2(auth_src, home / "auth.json")
        env = base_env()
        env["TESSERA_HOME"] = str(home)
        for k in ("CONFIG", "DATA", "CACHE", "STATE"):
            d = root / "x" / k.lower()
            d.mkdir(parents=True)
            env[f"XDG_{k}_HOME"] = str(d)
        cmd = [str(self.bin), "--local", "--home", str(home), "--model", self.model, "run", "--json", "--yes", "--cwd", str(work)]
        if self.max_steps:
            cmd += ["--max-steps", str(self.max_steps)]
        if self.thinking:
            cmd += ["--thinking", self.thinking]
        cmd.append(task.prompt)
        rec = run_process(cmd, work, env, out_dir, time_limit, root, "events.jsonl", task.prompt)
        self._sync_auth(home / "auth.json", auth_src)
        return merge_parsed(rec, parse_tessera(out_dir / "events.jsonl"))

    def _sync_auth(self, used, saved):
        """A refreshed ChatGPT login goes back to the bench home (refresh tokens are single-use)."""
        if not used.exists() or not saved.exists():
            return
        with self.auth_lock:
            if used.read_bytes() == saved.read_bytes():
                return

            def expiry(p):
                try:
                    data = json.loads(p.read_text())
                    return max(int(v.get("expires", 0)) for v in data.get("providers", {}).values() if isinstance(v, dict))
                except (ValueError, OSError, AttributeError):
                    return 0

            if expiry(used) > expiry(saved):  # only a refreshed login comes back
                tmp = saved.with_suffix(".tmp-bench")
                shutil.copy2(used, tmp)
                os.chmod(tmp, 0o600)
                os.replace(tmp, saved)


class IsolatedHarness(Harness):
    """Another agent harness, run on the local model with a throwaway HOME.

    Everything the tool reads or writes as configuration, sessions or caches
    lives in a fresh folder under BENCH_TOOLS/runs (outside /tmp: Codex
    refuses to set itself up under a temp dir), deleted after the run, so the
    user's own ~/.claude*, ~/.codex, ~/.hermes, ~/.omp and ~/.config are never
    read or written. The real toolchains stay usable (cargo, rustup, the
    user's Python packages, node). Each tool runs with its own defaults, every
    action auto-approved, on the same model, prompt and time limit.
    """

    name = "?"

    def __init__(self, args):
        if not (args.model or "").startswith("local/"):
            sys.exit(f"--harness {self.name} runs on the local model: --model local/<id>")
        self.model = args.model
        self.model_id = args.model.split("/", 1)[1]
        self.url = args.local_url.rstrip("/")
        self.ctx = local_context(self.url, self.model_id)
        self.version = self.tool_version()

    def serial(self):
        return True

    def tool_version(self):
        return ""

    def settings(self):
        """What the tool is configured with (recorded in meta.json)."""
        return {}

    def describe(self):
        return {"harness": self.name, "model": self.model, "version": self.version, "context_window": self.ctx, "settings": self.settings()}

    def label(self):
        return f"{self.name} {self.model}"

    def env(self, hroot):
        env = base_env()
        home = hroot / "home"
        home.mkdir(parents=True)
        env["HOME"] = str(home)
        for k in ("CONFIG", "DATA", "CACHE", "STATE"):
            d = hroot / "xdg" / k.lower()
            d.mkdir(parents=True)
            env[f"XDG_{k}_HOME"] = str(d)
        # The machine's toolchains, as the user's own shell would find them.
        env["CARGO_HOME"] = str(REAL_HOME / ".cargo")
        env["RUSTUP_HOME"] = str(REAL_HOME / ".rustup")
        env["PYTHONUSERBASE"] = str(REAL_HOME / ".local")
        return env

    def command(self, task, work, hroot, env, out_dir):
        raise NotImplementedError

    def parse(self, out_dir, hroot):
        raise NotImplementedError

    def run(self, task, work, root, out_dir, time_limit):
        hroot = root / "t"
        hroot.mkdir()
        try:
            env = self.env(hroot)
            cmd = self.command(task, work, hroot, env, out_dir)
            rec = run_process(cmd, work, env, out_dir, time_limit, root, "events.jsonl", task.prompt)
            rec["leftover_processes_killed"] += kill_leftovers(hroot)
            return merge_parsed(rec, self.parse(out_dir, hroot))
        finally:
            kill_leftovers(hroot)
            make_readable(hroot)
            shutil.rmtree(hroot, ignore_errors=True)


class ClaudeHarness(IsolatedHarness):
    """Claude Code: the user's subscription (--claude-model), or the local
    model through llama.cpp's Anthropic-compatible /v1/messages (--model local/…)."""

    name = "claude-code"

    def __init__(self, args):
        self.bin = shutil.which(args.claude_bin) or args.claude_bin
        self.flags = shlex.split(args.claude_flags)
        self.local = (args.model or "").startswith("local/")
        if self.local:
            super().__init__(args)
        else:
            if not args.claude_model:
                sys.exit("--harness claude-code needs --claude-model (subscription) or --model local/<id>")
            self.model = args.claude_model
            self.version = self.tool_version()

    def tool_version(self):
        return subprocess.run([self.bin, "--version"], capture_output=True, text=True).stdout.strip()

    def serial(self):
        return self.local

    def describe(self):
        d = {"harness": "claude-code", "model": self.model, "claude_version": self.version, "flags": self.flags}
        if self.local:
            d.update(context_window=self.ctx, base_url=self.base_url(),
                     adapter="bench/lib/msgshim.py: system blocks joined, in-conversation system messages as user text, model pinned")
        return d

    def base_url(self):
        return self.url[: -len("/v1")] if self.url.endswith("/v1") else self.url

    def command(self, task, work, hroot, env, out_dir):
        model = self.model_id if self.local else self.model
        if self.local:
            env["CLAUDE_CONFIG_DIR"] = str(hroot / "claude")
            env["ANTHROPIC_BASE_URL"] = self.start_shim(hroot)
            env["ANTHROPIC_AUTH_TOKEN"] = "bench-local"  # llama.cpp needs none; Claude Code wants one
            # Every role on the one local model (it is the only one the server may serve).
            for k in ("ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_OPUS_MODEL", "ANTHROPIC_DEFAULT_SONNET_MODEL",
                      "ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_SMALL_FAST_MODEL", "CLAUDE_CODE_SUBAGENT_MODEL"):
                env[k] = self.model_id
            env["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"] = "1"
            env["DISABLE_AUTOUPDATER"] = "1"
        return [
            self.bin, "-p", task.prompt,
            "--output-format", "stream-json", "--verbose",
            "--model", model,
            "--permission-mode", "bypassPermissions",
            "--no-session-persistence",
            *self.flags,
        ]

    def parse(self, out_dir, hroot):
        return parse_claude(out_dir / "events.jsonl")

    def start_shim(self, hroot):
        """bench/lib/msgshim.py between Claude Code and llama.cpp (see there why)."""
        port_file = hroot / "shim.port"
        self.shim = subprocess.Popen(
            [sys.executable, str(LIB / "msgshim.py"), self.base_url(), self.model_id, str(port_file)],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=open(hroot / "shim.log", "wb"), start_new_session=True,
        )
        for _ in range(100):
            if port_file.exists():
                return f"http://127.0.0.1:{port_file.read_text().strip()}"
            time.sleep(0.05)
        raise RuntimeError("the Claude Code protocol adapter didn't start")

    def run(self, task, work, root, out_dir, time_limit):
        if self.local:
            self.shim = None
            try:
                return super().run(task, work, root, out_dir, time_limit)
            finally:
                if self.shim:
                    self.shim.kill()
                    self.shim.wait()
        # The subscription: the user's own signed-in Claude Code, real HOME.
        env = base_env()
        cmd = self.command(task, work, None, env, out_dir)
        rec = run_process(cmd, work, env, out_dir, time_limit, root, "events.jsonl", task.prompt)
        return merge_parsed(rec, parse_claude(out_dir / "events.jsonl"))


class CodexHarness(IsolatedHarness):
    """OpenAI Codex CLI (`codex exec`) with a llama.cpp provider. Codex only
    speaks the Responses API (`wire_api = "chat"` was removed)."""

    name = "codex"

    def tool_version(self):
        return subprocess.run([str(TOOLS / "npm/node_modules/.bin/codex"), "--version"], capture_output=True, text=True).stdout.strip()

    def config(self):
        return (
            f'model = "{self.model_id}"\n'
            'model_provider = "llamacpp"\n'
            f"model_context_window = {self.ctx}\n"
            "[model_providers.llamacpp]\n"
            'name = "llama.cpp"\n'
            f'base_url = "{self.url}"\n'
            'wire_api = "responses"\n'
        )

    def settings(self):
        return {"config.toml": self.config(), "flags": ["exec", "--json", "--skip-git-repo-check", "--dangerously-bypass-approvals-and-sandbox"]}

    def command(self, task, work, hroot, env, out_dir):
        home = hroot / "codex"
        home.mkdir()
        (home / "config.toml").write_text(self.config())
        env["CODEX_HOME"] = str(home)
        return [str(TOOLS / "npm/node_modules/.bin/codex"), "exec", "--json", "--skip-git-repo-check",
                "--dangerously-bypass-approvals-and-sandbox", task.prompt]

    def parse(self, out_dir, hroot):
        out = {"tool_calls": 0, "tool_errors": 0, "tool_names": {}, "assistant_messages": 0, "compactions": 0, "notices": [], "steps": None}
        names = Counter()
        tokens = {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0}
        final, failed, turns = "", None, 0
        for ev in read_jsonl(out_dir / "events.jsonl"):
            t = ev.get("type")
            item = ev.get("item") or {}
            if t == "item.completed":
                kind = item.get("type") or item.get("item_type")
                if kind == "agent_message":
                    out["assistant_messages"] += 1
                    final = item.get("text") or final
                elif kind in ("command_execution", "file_change", "mcp_tool_call", "web_search"):
                    names[kind] += 1
                    if item.get("status") == "failed" or (kind == "command_execution" and item.get("exit_code") not in (0, None)):
                        out["tool_errors"] += 1
            elif t == "turn.completed":
                turns += 1
                u = ev.get("usage") or {}
                cached = u.get("cached_input_tokens", 0) or 0
                tokens["input"] += max(0, (u.get("input_tokens", 0) or 0) - cached)
                tokens["cache_read"] += cached
                tokens["output"] += u.get("output_tokens", 0) or 0
            elif t in ("turn.failed", "error"):
                failed = (ev.get("error") or {}).get("message") if isinstance(ev.get("error"), dict) else ev.get("message")
                out["notices"].append(f"error: {str(failed)[:300]}")
        out.update(tool_calls=sum(names.values()), tool_names=dict(names.most_common()), final_text=final, tokens=tokens, models_used=[self.model])
        out["status"] = "error" if failed and not final else ("finished" if turns else "crash")
        return out


class OpencodeHarness(IsolatedHarness):
    """opencode (`opencode run`) with an OpenAI-compatible llama.cpp provider.
    Its package cache is shared between runs (it installs the provider's npm
    package on first use); its sessions and data are per run. LSP servers are
    used when on PATH but not downloaded (as Tessera does)."""

    name = "opencode"

    def tool_version(self):
        return subprocess.run([str(TOOLS / "npm/node_modules/.bin/opencode"), "--version"], capture_output=True, text=True,
                              env=dict(base_env(), HOME=str(TOOLS / "opencode-home"))).stdout.strip()

    def config(self):
        return {
            "$schema": "https://opencode.ai/config.json",
            "model": f"llamacpp/{self.model_id}",
            "small_model": f"llamacpp/{self.model_id}",
            "autoupdate": False,
            "share": "disabled",
            "provider": {
                "llamacpp": {
                    "npm": "@ai-sdk/openai-compatible",
                    "name": "llama.cpp (local)",
                    "options": {"baseURL": self.url},
                    "models": {self.model_id: {"name": self.model_id, "tool_call": True, "reasoning": True,
                                               "limit": {"context": self.ctx, "output": 32768}}},
                }
            },
        }

    def settings(self):
        return {"opencode.json": self.config(), "flags": ["run", "--format", "json", "--auto"], "env": {"OPENCODE_DISABLE_LSP_DOWNLOAD": "true"}}

    def command(self, task, work, hroot, env, out_dir):
        cfg = Path(env["XDG_CONFIG_HOME"]) / "opencode"
        cfg.mkdir(parents=True)
        (cfg / "opencode.json").write_text(json.dumps(self.config(), indent=1))
        env["XDG_CACHE_HOME"] = str(TOOLS / "opencode-cache")
        env["OPENCODE_DISABLE_LSP_DOWNLOAD"] = "true"
        env["OPENCODE_DISABLE_AUTOUPDATE"] = "true"
        return [str(TOOLS / "npm/node_modules/.bin/opencode"), "run", "--format", "json", "--auto",
                "-m", f"llamacpp/{self.model_id}", "--dir", str(work), task.prompt]

    def parse(self, out_dir, hroot):
        out = {"tool_calls": 0, "tool_errors": 0, "tool_names": {}, "assistant_messages": 0, "compactions": 0, "notices": []}
        names = Counter()
        tokens = {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0}
        texts, steps, errors = [], 0, []
        for ev in read_jsonl(out_dir / "events.jsonl"):
            t = ev.get("type")
            part = ev.get("part") or {}
            if t == "step_finish" or part.get("type") == "step-finish":
                steps += 1
                tk = part.get("tokens") or {}
                cache = tk.get("cache") or {}
                tokens["input"] += tk.get("input", 0) or 0
                tokens["output"] += (tk.get("output", 0) or 0) + (tk.get("reasoning", 0) or 0)
                tokens["cache_read"] += cache.get("read", 0) or 0
                tokens["cache_write"] += cache.get("write", 0) or 0
            elif t == "tool_use" or part.get("type") == "tool":
                names[part.get("tool", "?")] += 1
                if (part.get("state") or {}).get("status") == "error":
                    out["tool_errors"] += 1
            elif t == "text" or part.get("type") == "text":
                if (part.get("text") or "").strip():
                    texts.append(part["text"])
            elif t == "error":
                errors.append(json.dumps(ev.get("error") or ev)[:300])
        out.update(tool_calls=sum(names.values()), tool_names=dict(names.most_common()), steps=steps or None, tokens=tokens,
                   final_text=texts[-1] if texts else "", assistant_messages=steps, models_used=[self.model])
        out["notices"] += [f"error: {e}" for e in errors]
        out["status"] = "error" if errors and not texts else ("finished" if steps or texts else "crash")
        return out


class OmpHarness(IsolatedHarness):
    """oh-my-pi (`omp -p --mode json`, on bun) with the llama.cpp server as an
    OpenAI-compatible provider. Its default approval mode is yolo."""

    name = "omp"

    def bun_env(self):
        return dict(base_env(), PATH=f"{TOOLS / 'bin'}:{os.environ.get('PATH', '')}", HOME=str(TOOLS / "omp-home"))

    def tool_version(self):
        (TOOLS / "omp-home").mkdir(exist_ok=True)
        return subprocess.run([str(TOOLS / "omp/node_modules/.bin/omp"), "--version"], capture_output=True, text=True, env=self.bun_env()).stdout.strip()

    def models_yml(self):
        return (
            "providers:\n"
            "  llama.cpp:\n"
            f"    baseUrl: {self.url}\n"
            "    auth: none\n"
            "    api: openai-completions\n"
            "    models:\n"
            f"      - id: {self.model_id}\n"
            f"        name: {self.model_id}\n"
            "        reasoning: true\n"
            "        input: [text]\n"
            f"        contextWindow: {self.ctx}\n"
            "        maxTokens: 32768\n"
        )

    def settings(self):
        return {"models.yml": self.models_yml(), "flags": ["-p", "--mode", "json", "--model", f"llama.cpp/{self.model_id}", "--no-session"]}

    def command(self, task, work, hroot, env, out_dir):
        agent = Path(env["HOME"]) / ".omp" / "agent"
        agent.mkdir(parents=True)
        (agent / "models.yml").write_text(self.models_yml())
        env["PATH"] = f"{TOOLS / 'bin'}:{env.get('PATH', '')}"
        return [str(TOOLS / "omp/node_modules/.bin/omp"), "-p", "--mode", "json", "--model", f"llama.cpp/{self.model_id}",
                "--no-session", task.prompt]

    def parse(self, out_dir, hroot):
        out = {"tool_calls": 0, "tool_errors": 0, "tool_names": {}, "assistant_messages": 0, "compactions": 0, "notices": []}
        names = Counter()
        tokens = {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0}
        final, errors, ended = "", [], False
        for ev in read_jsonl(out_dir / "events.jsonl"):
            t = ev.get("type")
            if t == "message_end":
                msg = ev.get("message") or {}
                if msg.get("role") == "assistant":
                    out["assistant_messages"] += 1
                    u = msg.get("usage") or {}
                    tokens["input"] += u.get("input", 0) or 0
                    tokens["output"] += u.get("output", 0) or 0
                    tokens["cache_read"] += u.get("cacheRead", 0) or 0
                    tokens["cache_write"] += u.get("cacheWrite", 0) or 0
                    text = "".join(c.get("text", "") for c in msg.get("content") or [] if isinstance(c, dict) and c.get("type") == "text")
                    if text.strip():
                        final = text
                    if msg.get("stopReason") == "error" or msg.get("errorMessage"):
                        errors.append(str(msg.get("errorMessage"))[:300])
            elif t == "tool_execution_end":
                names[ev.get("toolName", "?")] += 1
                if ev.get("isError"):
                    out["tool_errors"] += 1
            elif t in ("auto_compaction_end", "compaction_end"):
                out["compactions"] += 1
            elif t == "agent_end":
                ended = True
        out.update(tool_calls=sum(names.values()), tool_names=dict(names.most_common()), steps=out["assistant_messages"] or None,
                   tokens=tokens, final_text=final, models_used=[self.model])
        out["notices"] += [f"error: {e}" for e in errors]
        out["status"] = "finished" if ended and not (errors and not final) else ("error" if errors else "crash")
        return out


class HermesHarness(IsolatedHarness):
    """Hermes Agent (`hermes -z`, one-shot: approvals bypassed by design) with
    a custom OpenAI-compatible endpoint, in its own HERMES_HOME (never the
    user's ~/.hermes). Tokens and API calls come from --usage-file, tool calls
    from its session store."""

    name = "hermes"

    def tool_version(self):
        return subprocess.run([str(TOOLS / "hermes-venv/bin/python"), "-c", "import importlib.metadata as m; print('hermes-agent', m.version('hermes-agent'))"],
                              capture_output=True, text=True).stdout.strip()

    def config_yaml(self):
        return (
            "model:\n"
            f"  default: {self.model_id}\n"
            "  provider: custom\n"
            f"  base_url: {self.url}\n"
            "custom_providers:\n"
            "  - name: llama.cpp\n"
            f"    base_url: {self.url}\n"
            "    models:\n"
            f"      {self.model_id}:\n"
            f"        context_length: {self.ctx}\n"
        )

    def settings(self):
        return {"config.yaml": self.config_yaml(), "flags": ["-z", "<prompt>", "--usage-file", "usage.json"]}

    def command(self, task, work, hroot, env, out_dir):
        hh = hroot / "hermes"
        hh.mkdir()
        (hh / "config.yaml").write_text(self.config_yaml())
        env["HERMES_HOME"] = str(hh)
        env["PATH"] = f"{TOOLS / 'hermes-venv/bin'}:{env.get('PATH', '')}"
        return [str(TOOLS / "hermes-venv/bin/hermes"), "-z", task.prompt, "--usage-file", str(out_dir / "usage.json")]

    def parse(self, out_dir, hroot):
        import sqlite3

        out = {"tool_calls": 0, "tool_errors": 0, "tool_names": {}, "assistant_messages": 0, "compactions": 0, "notices": []}
        final = (out_dir / "events.jsonl").read_text(errors="replace").strip()
        usage = {}
        if (out_dir / "usage.json").exists():
            try:
                usage = json.loads((out_dir / "usage.json").read_text())
            except ValueError:
                pass
        names = Counter()
        db = hroot / "hermes" / "state.db"
        if db.exists():
            try:
                con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
                rows = con.execute("SELECT role, content, tool_calls, tool_name FROM messages ORDER BY id").fetchall()
                con.close()
                with open(out_dir / "hermes-messages.jsonl", "w") as f:
                    for role, content, tool_calls, tool_name in rows:
                        f.write(json.dumps({"role": role, "content": content, "tool_calls": tool_calls, "tool_name": tool_name}) + "\n")
                        if role == "assistant":
                            out["assistant_messages"] += 1
                            try:
                                for call in json.loads(tool_calls or "[]") or []:
                                    fn = (call.get("function") or {}).get("name") or call.get("name") or "?"
                                    names[fn] += 1
                            except (ValueError, AttributeError):
                                pass
                        elif role == "tool" and content and re.match(r'\s*\{\s*"(error|success"\s*:\s*false)', content):
                            out["tool_errors"] += 1
            except sqlite3.Error as e:
                out["notices"].append(f"warning: session store unreadable: {e}")
        out.update(
            tool_calls=sum(names.values()), tool_names=dict(names.most_common()), final_text=final,
            steps=usage.get("api_calls"), models_used=[f"{usage.get('provider')}/{usage.get('model')}"] if usage else [self.model],
            tokens={"input": usage.get("input_tokens") or 0, "output": usage.get("output_tokens") or 0,
                    "cache_read": usage.get("cache_read_tokens") or 0, "cache_write": usage.get("cache_write_tokens") or 0},
        )
        if usage.get("failure"):
            out["notices"].append(f"error: {str(usage['failure'])[:300]}")
        # A failed API call comes back as the "answer" (e.g. "HTTP 400: …").
        out["status"] = "error" if usage.get("failed") else ("finished" if final else "crash")
        return out


HARNESSES = {"tessera": None, "claude-code": ClaudeHarness, "codex": CodexHarness, "opencode": OpencodeHarness, "omp": OmpHarness, "hermes": HermesHarness}


def read_jsonl(path):
    try:
        f = open(path, "rb")
    except OSError:
        return
    with f:
        for raw in f:
            try:
                ev = json.loads(raw)
            except ValueError:
                continue
            if isinstance(ev, dict):
                yield ev


def parse_tuning(text):
    """'small=on,sampling=false,context_budget=0' -> {"small": "on", "sampling": False, "context_budget": 0}."""
    out = {}
    for item in filter(None, (x.strip() for x in text.split(","))):
        key, sep, value = item.partition("=")
        if not sep or not re.fullmatch(r"[a-z_]+", key.strip()):
            sys.exit(f"--tessera-tuning wants key=value pairs, got {item!r}")
        v = value.strip()
        if v in ("true", "false"):
            out[key.strip()] = v == "true"
        elif re.fullmatch(r"-?\d+", v):
            out[key.strip()] = int(v)
        elif re.fullmatch(r"-?\d+\.\d*", v):
            out[key.strip()] = float(v)
        else:
            out[key.strip()] = v
    return out


def toml_value(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float)):
        return str(v)
    return json.dumps(str(v))


def merge_parsed(rec, parsed):
    """The parsed log's fields, except that a run we stopped stays a timeout."""
    if rec.get("status") == "timeout":
        parsed["status"] = "timeout"
    rec.update(parsed)
    return rec


def local_context(url, model_id):
    """The context size the llama.cpp router starts this model with (GET /v1/models only)."""
    try:
        with urllib.request.urlopen(url.rstrip("/") + "/models", timeout=10) as r:
            data = json.load(r)
    except (OSError, ValueError) as e:
        sys.exit(f"the local model server at {url} doesn't answer ({e}); is the PC on?")
    for m in data.get("data", []):
        if m.get("id") == model_id or model_id in m.get("aliases", []):
            args = (m.get("status") or {}).get("args") or []
            for i, a in enumerate(args):
                if a in ("--ctx-size", "-c") and i + 1 < len(args):
                    return int(args[i + 1])
            meta = m.get("meta") or {}
            if meta.get("n_ctx_train"):
                return int(meta["n_ctx_train"])
            return 131072
    sys.exit(f"{model_id} is not on the server at {url}")


def run_process(cmd, work, env, out_dir, time_limit, root, events_name, prompt):
    """Run an agent; stdout to the event log, stderr to stderr.log."""
    (out_dir / "command.txt").write_text(shlex.join("<prompt>" if a == prompt else a for a in cmd) + "\n")
    events = out_dir / events_name
    t0 = time.time()
    status = None
    with open(events, "wb") as so, open(out_dir / "stderr.log", "wb") as se:
        p = subprocess.Popen(cmd, cwd=str(work), env=env, stdin=subprocess.DEVNULL, stdout=so, stderr=se, start_new_session=True)
        try:
            code = p.wait(timeout=time_limit)
        except subprocess.TimeoutExpired:
            status = "timeout"
            kill_tree(p.pid)
            code = p.wait()
        except KeyboardInterrupt:
            kill_tree(p.pid)
            kill_leftovers(work, root)
            raise
    wall = time.time() - t0
    leftovers = kill_leftovers(work, root)
    rec = {"exit_code": code, "wall_s": round(wall, 1), "leftover_processes_killed": leftovers}
    if status:
        rec["status"] = status
    return rec


def parse_tessera(path):
    out = {"tool_calls": 0, "tool_errors": 0, "tool_names": {}, "assistant_messages": 0, "compactions": 0, "notices": [], "models_used": []}
    names, models = Counter(), Counter()
    result = None
    with open(path, "rb") as f:
        for raw in f:
            try:
                ev = json.loads(raw)
            except ValueError:
                continue
            t = ev.get("type")
            if t == "result":
                result = ev
            elif t == "entry_added":
                e = ev.get("entry", {})
                k = e.get("kind")
                if k == "assistant":
                    out["assistant_messages"] += 1
                    m = e.get("model")
                    if isinstance(m, dict):
                        m = f"{m.get('provider')}/{m.get('model')}"
                    models[m if isinstance(m, str) else json.dumps(m)] += 1
                    for part in e.get("content", []):
                        if part.get("type") == "tool_call":
                            names[part.get("name", "?")] += 1
                elif k == "tool_result" and e.get("is_error"):
                    out["tool_errors"] += 1
                elif k == "compaction":
                    out["compactions"] += 1
                elif k == "notice" and e.get("level") in ("warning", "error"):
                    out["notices"].append(f"{e.get('level')}: {str(e.get('text', ''))[:300]}")
    out["tool_calls"] = sum(names.values())
    out["tool_names"] = dict(names.most_common())
    out["models_used"] = list(models)
    if result:
        u = result.get("usage") or {}
        out.update(
            status=result.get("status"),
            steps=result.get("steps"),
            final_text=result.get("text") or "",
            error=result.get("error"),
            tokens={
                "input": u.get("input", 0),
                "output": u.get("output", 0),
                "cache_read": u.get("cache_read", 0),
                "cache_write": u.get("cache_write", 0),
            },
        )
    else:
        out.setdefault("status", "crash")
        out["final_text"] = ""
    return out


def parse_claude(path):
    out = {"tool_calls": 0, "tool_errors": 0, "tool_names": {}, "assistant_messages": 0, "compactions": 0, "notices": []}
    names = Counter()
    messages = set()
    result = None
    last_text = ""
    with open(path, "rb") as f:
        for raw in f:
            try:
                ev = json.loads(raw)
            except ValueError:
                continue
            t = ev.get("type")
            if t == "result":
                result = ev
            elif t == "assistant" and not ev.get("parent_tool_use_id"):
                msg = ev.get("message") or {}
                messages.add(msg.get("id") or len(messages))
                for part in msg.get("content", []):
                    if part.get("type") == "tool_use":
                        names[part.get("name", "?")] += 1
                    elif part.get("type") == "text" and part.get("text", "").strip():
                        last_text = part["text"]
            elif t == "user":
                for part in (ev.get("message") or {}).get("content", []) or []:
                    if isinstance(part, dict) and part.get("type") == "tool_result" and part.get("is_error"):
                        out["tool_errors"] += 1
            elif t == "system" and ev.get("subtype") == "compact_boundary":
                out["compactions"] += 1
    out["tool_calls"] = sum(names.values())
    out["tool_names"] = dict(names.most_common())
    out["assistant_messages"] = len(messages)
    if result:
        tokens = {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0}
        mu = result.get("modelUsage") or {}
        if mu:
            for m in mu.values():
                tokens["input"] += m.get("inputTokens", 0)
                tokens["output"] += m.get("outputTokens", 0)
                tokens["cache_read"] += m.get("cacheReadInputTokens", 0)
                tokens["cache_write"] += m.get("cacheCreationInputTokens", 0)
            out["models_used"] = list(mu)
        else:
            u = result.get("usage") or {}
            tokens = {
                "input": u.get("input_tokens", 0),
                "output": u.get("output_tokens", 0),
                "cache_read": u.get("cache_read_input_tokens", 0),
                "cache_write": u.get("cache_creation_input_tokens", 0),
            }
        sub = result.get("subtype", "")
        out.update(
            status="finished" if sub == "success" and not result.get("is_error") else ("error" if sub == "success" else sub or "error"),
            steps=result.get("num_turns"),
            final_text=result.get("result") or last_text,
            tokens=tokens,
            cost_usd=result.get("total_cost_usd"),
            api_s=round((result.get("duration_api_ms") or 0) / 1000, 1),
        )
    else:
        out.update(status="crash", final_text=last_text)
    return out


# ---- running -----------------------------------------------------------------


def run_one(task, rep, harness, args, out_root):
    out_dir = out_root / task.id / f"r{rep}"
    if (out_dir / "result.json").exists() and not args.force:
        return json.loads((out_dir / "result.json").read_text())
    if out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True)
    (out_dir / "prompt.txt").write_text(task.prompt + "\n")
    work = Path(tempfile.mkdtemp(prefix="tessera-bench-", dir=args.workroot))
    root = new_scratch()
    started = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
    rec = {
        "task": task.id,
        "repeat": rep,
        "harness": harness.name,
        "model": harness.model,
        "label": args.label or harness.label(),
        "difficulty": task.difficulty,
        "category": task.category,
        "tags": task.tags,
        "started_at": started,
    }
    try:
        prepare(task, work)
        snapshot(work, root / "p")
        limit = int(task.time_limit * args.timeout_scale)
        rec["time_limit_s"] = limit
        rec.update(harness.run(task, work, root, out_dir, limit))
        (out_dir / "answer.txt").write_text(rec.get("final_text") or "", encoding="utf-8")
        passed, output, check_s = run_check(task, work, rec.get("final_text") or "", out_dir)
        rec["pass"] = passed
        rec["check_output"] = output[-4000:]
        rec["check_s"] = round(check_s, 1)
        make_readable(work)  # after the check: it may look at permissions
        (out_dir / "work.diff").write_text(work_diff(root / "p", work), encoding="utf-8")
    except Exception as e:  # a broken run is recorded, not fatal
        rec.setdefault("status", "crash")
        rec["pass"] = False
        rec["error"] = f"{type(e).__name__}: {e}"
    finally:
        kill_leftovers(work, root)
        for d in (work, root):
            make_readable(d)
        if args.keep_work:
            rec["work_dir"], rec["scratch_dir"] = str(work), str(root)
        else:
            shutil.rmtree(work, ignore_errors=True)
            shutil.rmtree(root, ignore_errors=True)
    ev = out_dir / "events.jsonl"
    if ev.exists():
        with open(ev, "rb") as fi, gzip.open(str(ev) + ".gz", "wb") as fo:
            shutil.copyfileobj(fi, fo)
        ev.unlink()
    tmp = out_dir / "result.json.tmp"
    tmp.write_text(json.dumps(rec, indent=1, ensure_ascii=False))
    tmp.replace(out_dir / "result.json")
    with print_lock, open(out_root / "runs.jsonl", "a") as f:
        f.write(json.dumps({k: v for k, v in rec.items() if k not in ("check_output", "final_text")}, ensure_ascii=False) + "\n")
    tok = rec.get("tokens") or {}
    log(
        f"{'PASS' if rec['pass'] else 'FAIL'}  {task.id} r{rep}  {rec.get('status', '?')}  "
        f"{rec.get('wall_s', 0):.0f}s  steps={rec.get('steps')}  tools={rec.get('tool_calls')}  "
        f"in={tok.get('input', 0) + tok.get('cache_read', 0) + tok.get('cache_write', 0)} out={tok.get('output', 0)}"
    )
    return rec


def git_rev():
    try:
        return subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=BENCH, capture_output=True, text=True).stdout.strip()
    except OSError:
        return ""


def take_model_lock(args):
    """Hold the local model's lock for the whole run (its server has one slot,
    and live tests of other work use it too). Same lock as `flock FILE …`."""
    import fcntl

    if not (args.model or "").startswith("local/") or not args.model_lock:
        return None
    # Close-on-exec: the agents and servers started during the run must not
    # inherit the lock (a leftover child would hold it forever).
    fd = os.open(args.model_lock, os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_CLOEXEC, 0o666)
    f = os.fdopen(fd, "a")
    assert not os.get_inheritable(fd)
    try:
        fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        log(f"waiting for {args.model_lock} (something else is using the local model)…")
        fcntl.flock(f, fcntl.LOCK_EX)
    return f  # released when the process exits


def cmd_bench(args):
    tasks = load_tasks(split_patterns(args.tasks))
    if not tasks:
        sys.exit("no tasks match")
    lock = take_model_lock(args)  # before the first request to the local server
    harness = TesseraHarness(args) if args.harness == "tessera" else HARNESSES[args.harness](args)
    harness.lock = lock
    out_root = Path(args.out).expanduser().resolve()
    out_root.mkdir(parents=True, exist_ok=True)
    jobs = args.jobs
    if jobs > 1 and harness.serial():
        log(f"note: {harness.label()} runs one task at a time (single-slot server or one shared login); --jobs {jobs} ignored")
        jobs = 1
    meta = dict(
        harness.describe(),
        label=args.label or harness.label(),
        bench_commit=git_rev(),
        host=platform.node(),
        started_at=datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        tasks=[t.id for t in tasks],
        repeat=args.repeat,
        timeout_scale=args.timeout_scale,
    )
    meta_file = out_root / "meta.json"
    if meta_file.exists():
        old = json.loads(meta_file.read_text())
        for k in ("harness", "model", "tessera_sha256", "thinking", "max_steps", "tuning", "config", "flags", "version", "settings"):
            if old.get(k) != meta.get(k):
                sys.exit(f"{out_root} holds results of another setup ({k}: {old.get(k)!r} vs {meta.get(k)!r}); use another --out")
        meta["tasks"] = sorted(set(old.get("tasks", [])) | set(meta["tasks"]))
        meta["started_at"] = old.get("started_at", meta["started_at"])
        meta["updated_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
    meta_file.write_text(json.dumps(meta, indent=1))
    plan = [(t, r) for r in range(1, args.repeat + 1) for t in tasks]
    log(f"{harness.label()}: {len(tasks)} tasks x {args.repeat} = {len(plan)} runs, {jobs} at a time -> {out_root}")
    results = []
    if jobs == 1:
        for t, r in plan:
            results.append(run_one(t, r, harness, args, out_root))
    else:
        with concurrent.futures.ThreadPoolExecutor(jobs) as ex:
            for rec in ex.map(lambda tr: run_one(tr[0], tr[1], harness, args, out_root), plan):
                results.append(rec)
    passed = sum(1 for r in results if r.get("pass"))
    log(f"done: {passed}/{len(results)} passed. Summary: python3 bench/report.py {out_root}")


# ---- self-test -----------------------------------------------------------------


def selftest_one(task):
    problems = task.validate()
    row = {"task": task.id, "difficulty": task.difficulty, "category": task.category, "problems": problems}
    if problems:
        return row
    sol = SOLUTIONS / task.id
    for phase in ("untouched", "solved"):
        root = Path(tempfile.mkdtemp(prefix="tessera-bench-self-"))
        try:
            work = prepare(task, root)
            answer = ""
            if phase == "solved":
                solve = sol / "solve.sh"
                if not solve.exists():
                    row["solved"] = None
                    row["solved_out"] = "no solution"
                    continue
                env = dict(base_env(), **GIT_ID, BENCH_SOLUTION=str(sol), BENCH_TASK=str(task.dir), BENCH_WORK=str(work))
                sh(["sh", str(solve)], work, env, timeout=600)
                if (sol / "answer.txt").exists():
                    answer = (sol / "answer.txt").read_text()
            passed, output, secs = run_check(task, work, answer)
            row[phase] = passed
            row[phase + "_out"] = output
            row[phase + "_s"] = round(secs, 1)
        except Exception as e:
            row[phase] = "error"
            row[phase + "_out"] = f"{type(e).__name__}: {e}"
        finally:
            kill_leftovers(root)
            make_readable(root)
            shutil.rmtree(root, ignore_errors=True)
    return row


def cmd_selftest(args):
    tasks = load_tasks(split_patterns(args.tasks), default_tier=None)
    good = 0
    rows = []
    with concurrent.futures.ThreadPoolExecutor(max(1, args.jobs)) as ex:
        for row in ex.map(selftest_one, tasks):
            rows.append(row)
            ok = not row["problems"] and row.get("untouched") is False and row.get("solved") is True
            good += ok
            log(
                f"{'ok  ' if ok else 'BAD '} {row['task']:<28} {row['difficulty']:<6} "
                f"untouched={'FAIL' if row.get('untouched') is False else row.get('untouched')!s:<5} "
                f"solved={'PASS' if row.get('solved') is True else row.get('solved')!s:<5} "
                f"check {row.get('untouched_s', 0)}s/{row.get('solved_s', 0)}s"
            )
            if not ok:
                for p in row["problems"]:
                    log(f"     problem: {p}")
                if row.get("untouched") is not False:
                    log("     untouched files should FAIL, check said:\n" + _indent(row.get("untouched_out", "")))
                if row.get("solved") is not True:
                    log("     the reference solution should PASS, check said:\n" + _indent(row.get("solved_out", "")))
    counts = Counter(t.difficulty for t in tasks)
    log(f"self-test: {good}/{len(tasks)} tasks behave ({', '.join(f'{counts[d]} {d}' for d in DIFFICULTIES)})")
    return 0 if good == len(tasks) else 1


def _indent(s, n=2500):
    s = s[-n:]
    return "\n".join("       " + line for line in s.splitlines())


def cmd_recheck(args):
    """Run the current checks again on finished runs: the work dir is rebuilt
    from the task's files and the run's work.diff. Only for tasks whose check
    needs nothing the diff leaves out (git history, file modes, binary files)."""
    out_root = Path(args.recheck).expanduser().resolve()
    patterns = split_patterns(args.tasks)
    if not patterns:
        sys.exit("--recheck needs --tasks (only tasks whose checks don't depend on git history, file modes or binary files)")
    tasks = {t.id: t for t in load_tasks(patterns)}
    for res in sorted(out_root.glob("*/r*/result.json")):
        rec = json.loads(res.read_text())
        task = tasks.get(rec["task"])
        if task is None or rec.get("status") == "crash":
            continue
        diff = res.parent / "work.diff"
        root = Path(tempfile.mkdtemp(prefix="tessera-bench-recheck-", dir=args.workroot))
        try:
            work = prepare(task, root)
            if diff.exists() and diff.stat().st_size:
                p = subprocess.run(["patch", "-s", "-p1", "--batch", "-i", str(diff)], cwd=work, capture_output=True, text=True)
                if p.returncode != 0:
                    log(f"skip {task.id} {res.parent.name}: the diff doesn't apply ({p.stdout.strip()[:200]})")
                    continue
            answer = (res.parent / "answer.txt").read_text() if (res.parent / "answer.txt").exists() else ""
            passed, output, _ = run_check(task, work, answer)
        finally:
            kill_leftovers(root)
            make_readable(root)
            shutil.rmtree(root, ignore_errors=True)
        if passed != rec.get("pass"):
            rec.setdefault("before_recheck", {"pass": rec.get("pass"), "check_output": rec.get("check_output")})
            rec["pass"] = passed
            rec["check_output"] = output[-4000:]
            rec["rechecked"] = {"bench_commit": git_rev(), "at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")}
            (res.parent / "check.log").write_text(output)
            tmp = res.with_suffix(".json.tmp")
            tmp.write_text(json.dumps(rec, indent=1, ensure_ascii=False))
            tmp.replace(res)
        log(f"{'PASS' if passed else 'FAIL'}  {task.id} {res.parent.name}  (was {'PASS' if rec.get('before_recheck', rec).get('pass') else 'FAIL'})")


def cmd_list(args):
    tasks = load_tasks(split_patterns(args.tasks), default_tier=None)
    for t in tasks:
        print(f"{t.id:<28} {t.tier:<6} {t.difficulty:<6} {t.category:<14} {t.time_limit:>5}s  {t.title}")
    counts = Counter(t.difficulty for t in tasks)
    tiers = Counter(t.tier for t in tasks)
    print(f"{len(tasks)} tasks: " + ", ".join(f"{counts[d]} {d}" for d in DIFFICULTIES) + " (tiers: " + ", ".join(f"{n} {k}" for k, n in sorted(tiers.items())) + ")")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--list", action="store_true", help="list the tasks")
    ap.add_argument("--selftest", action="store_true", help="check every task: untouched files FAIL, the reference solution PASSES")
    ap.add_argument("--recheck", metavar="RESULTS", help="run today's checks on finished runs again (rebuilt from work.diff; needs --tasks)")
    ap.add_argument("--harness", choices=("tessera", "claude-code", "codex", "opencode", "omp", "hermes"))
    ap.add_argument("--tasks", action="append", help="ids or globs (comma-separated); also easy/medium/hard, tag:X, cat:X, tier:hard2, all (default: tier core)")
    ap.add_argument("--repeat", type=int, default=1)
    ap.add_argument("--jobs", type=int, default=1, help="runs at once (forced to 1 for the local model and ChatGPT)")
    ap.add_argument("--out", help="result folder (re-use it to continue an interrupted run)")
    ap.add_argument("--label", help="name shown in reports (default: harness and model)")
    ap.add_argument("--timeout-scale", type=float, default=1.0, help="multiply every task's time limit")
    ap.add_argument("--max-steps", type=int, default=100, help="tessera: stop a run after this many steps (0 = Tessera's own limit)")
    ap.add_argument("--keep-work", action="store_true", help="keep each run's temp folder (work dir, home)")
    ap.add_argument("--force", action="store_true", help="re-run runs that already have a result")
    ap.add_argument("--workroot", default=None, help="where temp work dirs go (default: the system temp dir)")
    g = ap.add_argument_group("tessera")
    g.add_argument("--tessera-bin", default=str(Path("target/release/tessera").expanduser()))
    g.add_argument("--model", help="provider/model, e.g. local/qwen3.8-27b or openai-codex/gpt-5.5 (other harnesses: local/<id> only)")
    g.add_argument("--thinking", choices=("off", "low", "medium", "high", "max"))
    g.add_argument("--local-url", default=DEFAULT_LOCAL_URL, help="OpenAI-compatible server for local/<model>")
    g.add_argument("--model-lock", default="/tmp/tessera-local-model.lock", help="lock file held for the whole run with a local/<model> (one slot, shared with live tests; '' = none)")
    g.add_argument("--bench-home", default=str(DEFAULT_BENCH_HOME), help="where the ChatGPT login (auth.json) and an optional bench-config.toml (TOML for every run) live")
    g.add_argument("--tessera-config", help="extra TOML appended to each run's config (for A/B of settings)")
    g.add_argument("--tessera-tuning", help='keys for [tuning."*"], e.g. small=on,sampling=false,parallel_tool_calls=true,context_budget=0 (check=false is always set)')
    g = ap.add_argument_group("claude-code")
    g.add_argument("--claude-bin", default="claude")
    g.add_argument("--claude-model", help="e.g. haiku, sonnet, opus")
    g.add_argument("--claude-flags", default="--safe-mode --strict-mcp-config", help="extra documented flags (default: no user customizations, no MCP servers)")
    args = ap.parse_args()

    if args.list:
        return cmd_list(args)
    if args.selftest:
        return sys.exit(cmd_selftest(args))
    if args.recheck:
        return cmd_recheck(args)
    if not args.harness:
        ap.error("choose --harness, --selftest or --list")
    if not args.out:
        ap.error("--out is required")
    return cmd_bench(args)


if __name__ == "__main__":
    main()
