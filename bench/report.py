#!/usr/bin/env python3
"""Summaries and A/B comparisons of benchmark results.

  bench/report.py RESULTS [RESULTS ...]     a summary per result folder
  bench/report.py --ab BEFORE AFTER         what changed between two folders
  add --json for machine-readable output

A result folder is what `bench/run.py --out` wrote (runs.jsonl and one
result.json per run). Pass rates are the mean over tasks of each task's pass
rate across repeats, so every task weighs the same however often it ran.
"""

import argparse
import json
import math
import random
import statistics
import sys
from collections import Counter, defaultdict
from pathlib import Path

DIFFICULTIES = ("easy", "medium", "hard")


def load(folder):
    folder = Path(folder)
    runs = []
    for p in sorted(folder.glob("*/r*/result.json")):
        try:
            runs.append(json.loads(p.read_text()))
        except ValueError:
            continue
    meta = {}
    if (folder / "meta.json").exists():
        meta = json.loads((folder / "meta.json").read_text())
    if not runs:
        sys.exit(f"no results in {folder}")
    return meta, runs


def label_of(meta, runs, folder):
    lab = meta.get("label") or runs[0].get("label") or Path(folder).name
    if meta.get("tessera_sha256"):
        lab += f" [{meta['tessera_sha256'][:8]}]"
    elif meta.get("version") or meta.get("claude_version"):
        import re as _re

        found = _re.search(r"\d+\.\d+[\w.\-]*", meta.get("version") or meta.get("claude_version"))
        if found:
            lab += f" [{found.group(0)}]"
    return lab


def per_task(runs):
    """task -> list of pass booleans (one per repeat)."""
    out = defaultdict(list)
    for r in runs:
        out[r["task"]].append(bool(r.get("pass")))
    return out


def rate(values):
    return sum(values) / len(values) if values else 0.0


def bootstrap_ci(task_rates, iters=4000, seed=1):
    """95% interval of the mean over tasks, resampling tasks."""
    rates = list(task_rates)
    if len(rates) < 2:
        return (float("nan"), float("nan"))
    rng = random.Random(seed)
    means = sorted(statistics.fmean(rng.choices(rates, k=len(rates))) for _ in range(iters))
    return means[int(0.025 * iters)], means[int(0.975 * iters) - 1]


def mean(xs):
    xs = [x for x in xs if x is not None]
    return statistics.fmean(xs) if xs else None


def median(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else None


def tok_in(r):
    t = r.get("tokens") or {}
    return t.get("input", 0) + t.get("cache_read", 0) + t.get("cache_write", 0)


def fmt(x, digits=0, suffix=""):
    if x is None or (isinstance(x, float) and math.isnan(x)):
        return "-"
    if digits == 0:
        return f"{x:,.0f}{suffix}"
    return f"{x:,.{digits}f}{suffix}"


def first_failure(r):
    if r.get("error"):
        return r["error"][:160]
    for line in (r.get("check_output") or "").splitlines():
        if line.startswith("FAIL:"):
            return line[5:].strip()[:160]
    return r.get("status") or "?"


def summary(folder):
    meta, runs = load(folder)
    tasks = per_task(runs)
    task_rates = {t: rate(v) for t, v in tasks.items()}
    by_task = {r["task"]: r for r in runs}
    lo, hi = bootstrap_ci(task_rates.values())
    s = {
        "folder": str(folder),
        "label": label_of(meta, runs, folder),
        "runs": len(runs),
        "tasks": len(tasks),
        "repeats": max(len(v) for v in tasks.values()),
        "pass_rate": statistics.fmean(task_rates.values()),
        "ci95": [lo, hi],
        "by_difficulty": {},
        "by_category": {},
        "status": dict(Counter(r.get("status") or "?" for r in runs)),
        "mean_steps": mean(r.get("steps") for r in runs),
        "mean_replies": mean(r.get("assistant_messages") for r in runs),
        "mean_tool_calls": mean(r.get("tool_calls") for r in runs),
        "tool_error_rate": None,
        "mean_tokens_in": mean(tok_in(r) for r in runs),
        "mean_tokens_out": mean((r.get("tokens") or {}).get("output") for r in runs),
        "cache_share": None,
        "mean_wall_s": mean(r.get("wall_s") for r in runs),
        "median_wall_s": median(r.get("wall_s") for r in runs),
        "total_wall_h": sum(r.get("wall_s") or 0 for r in runs) / 3600,
        "cost_usd": sum(r.get("cost_usd") or 0 for r in runs) or None,
        "failures": [],
        "flaky": sorted(t for t, v in tasks.items() if 0 < sum(v) < len(v)),
    }
    calls = sum(r.get("tool_calls") or 0 for r in runs)
    if calls:
        s["tool_error_rate"] = sum(r.get("tool_errors") or 0 for r in runs) / calls
    tin = sum(tok_in(r) for r in runs)
    if tin:
        s["cache_share"] = sum((r.get("tokens") or {}).get("cache_read", 0) for r in runs) / tin
    for key, field in (("by_difficulty", "difficulty"), ("by_category", "category")):
        groups = defaultdict(list)
        for t, rr in task_rates.items():
            groups[by_task[t].get(field, "?")].append(rr)
        s[key] = {g: {"tasks": len(v), "pass_rate": statistics.fmean(v)} for g, v in sorted(groups.items())}
    for r in sorted(runs, key=lambda r: (r["task"], r.get("repeat", 0))):
        if not r.get("pass"):
            s["failures"].append({"task": r["task"], "repeat": r.get("repeat"), "status": r.get("status"), "why": first_failure(r)})
    return s


def print_summary(s):
    print(f"== {s['label']}  ({s['folder']})")
    print(
        f"   pass rate {s['pass_rate'] * 100:.1f}%  (95% CI {fmt(s['ci95'][0] * 100)}-{fmt(s['ci95'][1] * 100, 0, '%')}, "
        f"{s['tasks']} tasks x {s['repeats']} repeats = {s['runs']} runs)"
    )
    diff = "  ".join(
        f"{d} {v['pass_rate'] * 100:.0f}% ({v['tasks']})" for d, v in ((d, s["by_difficulty"].get(d)) for d in DIFFICULTIES) if v
    )
    print(f"   by difficulty: {diff}")
    print("   by category:   " + "  ".join(f"{c} {v['pass_rate'] * 100:.0f}%" for c, v in s["by_category"].items()))
    print(
        f"   per run: {fmt(s['mean_steps'], 1)} steps, {fmt(s['mean_replies'], 1)} model replies, {fmt(s['mean_tool_calls'], 1)} tool calls"
        f" ({fmt(s['tool_error_rate'] and s['tool_error_rate'] * 100, 0, '%')} errors),"
        f" {fmt(s['mean_tokens_in'])} tokens in ({fmt(s['cache_share'] and s['cache_share'] * 100, 0, '%')} cached),"
        f" {fmt(s['mean_tokens_out'])} out, {fmt(s['mean_wall_s'])} s (median {fmt(s['median_wall_s'])} s)"
    )
    extra = f", cost {fmt(s['cost_usd'], 2)} USD (list price)" if s["cost_usd"] else ""
    print(f"   total time {s['total_wall_h']:.2f} h{extra}; endings: " + ", ".join(f"{k} {v}" for k, v in sorted(s["status"].items())))
    if s["flaky"]:
        print(f"   flaky (passed some repeats): {', '.join(s['flaky'])}")
    if s["failures"]:
        print("   failures:")
        for f in s["failures"]:
            print(f"     {f['task']:<26} r{f['repeat']} {str(f['status']):<12} {f['why']}")
    print()


def mcnemar_p(b, c):
    """Exact two-sided McNemar p-value (b and c are the discordant counts)."""
    n = b + c
    if n == 0:
        return 1.0
    k = min(b, c)
    tail = sum(math.comb(n, i) for i in range(k + 1)) / 2**n
    return min(1.0, 2 * tail)


def ab(folder_a, folder_b):
    meta_a, runs_a = load(folder_a)
    meta_b, runs_b = load(folder_b)
    ta, tb = per_task(runs_a), per_task(runs_b)
    common = sorted(set(ta) & set(tb))
    if not common:
        sys.exit("the two folders have no tasks in common")
    ra = {t: rate(ta[t]) for t in common}
    rb = {t: rate(tb[t]) for t in common}
    diffs = [rb[t] - ra[t] for t in common]
    rng = random.Random(7)
    boots = sorted(statistics.fmean(rng.choices(diffs, k=len(diffs))) for _ in range(4000))
    lo, hi = boots[100], boots[3899]
    # Two-sided bootstrap p-value for "no difference" (centered resamples).
    d0 = statistics.fmean(diffs)
    centered = [d - d0 for d in diffs]
    extreme = sum(1 for _ in range(4000) if abs(statistics.fmean(rng.choices(centered, k=len(centered)))) >= abs(d0) - 1e-12)
    p_boot = extreme / 4000
    one_a = all(len(ta[t]) == 1 for t in common)
    one_b = all(len(tb[t]) == 1 for t in common)
    b = sum(1 for t in common if ra[t] == 1 and rb[t] == 0)
    c = sum(1 for t in common if ra[t] == 0 and rb[t] == 1)
    sd = statistics.stdev(diffs) if len(diffs) > 1 else float("nan")
    # The smallest true difference this design would detect 80% of the time
    # (two-sided 5%): about 2.8 standard errors of the mean difference.
    mde = 2.8 * sd / math.sqrt(len(common)) if len(common) > 1 else float("nan")

    def eff(runs, tasks_ok):
        rs = [r for r in runs if r["task"] in tasks_ok and r.get("pass")]
        return {
            "steps": mean(r.get("steps") for r in rs),
            "replies": mean(r.get("assistant_messages") for r in rs),
            "tokens_in": mean(tok_in(r) for r in rs),
            "tokens_out": mean((r.get("tokens") or {}).get("output") for r in rs),
            "wall_s": mean(r.get("wall_s") for r in rs),
        }

    both = {t for t in common if ra[t] > 0 and rb[t] > 0}
    return {
        "a": label_of(meta_a, runs_a, folder_a),
        "b": label_of(meta_b, runs_b, folder_b),
        "tasks": len(common),
        "only_a": sorted(set(ta) - set(tb)),
        "only_b": sorted(set(tb) - set(ta)),
        "pass_a": statistics.fmean(ra.values()),
        "pass_b": statistics.fmean(rb.values()),
        "diff": d0,
        "ci95": [lo, hi],
        "p_bootstrap": p_boot,
        "mcnemar": {"a_only": b, "b_only": c, "p": mcnemar_p(b, c)} if one_a and one_b else None,
        "sd_task_diff": sd,
        "min_detectable_diff": mde,
        "flips": [{"task": t, "a": ra[t], "b": rb[t]} for t in common if ra[t] != rb[t]],
        "efficiency_on_tasks_both_solved": {"tasks": len(both), "a": eff(runs_a, both), "b": eff(runs_b, both)},
    }


def print_ab(r):
    pct = lambda x: f"{x * 100:+.1f}" if x is not None else "-"  # noqa: E731
    print(f"== A: {r['a']}\n   B: {r['b']}")
    print(f"   {r['tasks']} tasks in common; pass rate A {r['pass_a'] * 100:.1f}%  B {r['pass_b'] * 100:.1f}%")
    print(f"   B - A = {pct(r['diff'])} points (95% CI {pct(r['ci95'][0])} to {pct(r['ci95'][1])}; bootstrap p = {r['p_bootstrap']:.3f})")
    if r["mcnemar"]:
        m = r["mcnemar"]
        print(f"   McNemar: {m['a_only']} tasks only A solved, {m['b_only']} only B; exact p = {m['p']:.3f}")
    if not math.isnan(r["min_detectable_diff"]):
        print(
            f"   With this spread (SD of per-task differences {r['sd_task_diff'] * 100:.0f} points), differences under "
            f"~{r['min_detectable_diff'] * 100:.0f} points can't be told from noise; more repeats shrink it (about 1/sqrt(repeats))."
        )
    if r["only_a"] or r["only_b"]:
        print(f"   only in A: {', '.join(r['only_a']) or '-'}; only in B: {', '.join(r['only_b']) or '-'}")
    if r["flips"]:
        print("   tasks that changed (pass rate A -> B):")
        for f in sorted(r["flips"], key=lambda f: f["b"] - f["a"]):
            print(f"     {f['task']:<26} {f['a'] * 100:4.0f}% -> {f['b'] * 100:4.0f}%")
    e = r["efficiency_on_tasks_both_solved"]
    if e["tasks"]:
        a, b = e["a"], e["b"]
        print(f"   on the {e['tasks']} tasks both solved (means per passing run), A -> B:")
        print(
            f"     steps {fmt(a['steps'], 1)} -> {fmt(b['steps'], 1)}; model replies {fmt(a['replies'], 1)} -> {fmt(b['replies'], 1)}; tokens in {fmt(a['tokens_in'])} -> {fmt(b['tokens_in'])}; "
            f"out {fmt(a['tokens_out'])} -> {fmt(b['tokens_out'])}; time {fmt(a['wall_s'])} s -> {fmt(b['wall_s'])} s"
        )
    print()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("folders", nargs="*")
    ap.add_argument("--ab", nargs=2, metavar=("BEFORE", "AFTER"))
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()
    if args.ab:
        r = ab(*args.ab)
        print(json.dumps(r, indent=1)) if args.json else print_ab(r)
        return
    if not args.folders:
        ap.error("give result folders, or --ab BEFORE AFTER")
    out = [summary(f) for f in args.folders]
    if args.json:
        print(json.dumps(out, indent=1))
        return
    for s in out:
        print_summary(s)
    if len(out) > 1:
        print(f"{'':<44} {'pass':>6} {'easy':>5} {'med':>5} {'hard':>5} {'steps':>6} {'tok in':>9} {'tok out':>8} {'time':>6}")
        for s in out:
            d = s["by_difficulty"]
            g = lambda k: f"{d[k]['pass_rate'] * 100:.0f}%" if k in d else "-"  # noqa: E731
            print(
                f"{s['label'][:44]:<44} {s['pass_rate'] * 100:5.1f}% {g('easy'):>5} {g('medium'):>5} {g('hard'):>5} "
                f"{fmt(s['mean_steps'], 1):>6} {fmt(s['mean_tokens_in']):>9} {fmt(s['mean_tokens_out']):>8} {fmt(s['mean_wall_s']):>5}s"
            )


if __name__ == "__main__":
    main()
