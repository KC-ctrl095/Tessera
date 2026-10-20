// Settings → Usage: subscriptions first (their 5-hour and weekly limits),
// then what the models did over a period (`get_usage`): totals, a bar per day
// (stacked by model; tap or click one for its details), a table per model and
// a breakdown by kind of request. Costs are estimates from list prices the
// server knows, seen two ways: at pay-per-token (API) prices, or as what you
// pay with subscriptions (their plans, against what the same use would cost
// per token).
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import type { ModelUsage } from "../proto/ModelUsage";
import type { SubscriptionInfo } from "../proto/SubscriptionInfo";
import type { UsageReport } from "../proto/UsageReport";
import type { UsageTotals } from "../proto/UsageTotals";
import { call, modelLabel } from "../lib/store";
import { RpcError } from "../lib/transport";
import { errorMessage } from "../lib/ui";
import { fmtCost, fmtTokens, lsGet, lsSet, plural, utcOffsets } from "../lib/util";
import { Icon } from "./icons";
import { Segmented, Toggle } from "./primitives";
import { MONTH_MS, NoSubscriptions, SubscriptionCard, UnpricedNote, money } from "./settings/Pricing";
import "../styles/usage.css";

type Period = "today" | "7d" | "30d" | "all";
type Measure = "tokens" | "requests" | "cost";
/** Costs at pay-per-token prices, or what you pay (subscription plans plus per-token billing). */
type Pricing = "api" | "plans";

const PERIODS: { value: Period; label: string }[] = [
  { value: "today", label: "Today" },
  { value: "7d", label: "7 days" },
  { value: "30d", label: "30 days" },
  { value: "all", label: "All" },
];

const KIND_LABEL: Record<string, string> = {
  chat: "Chats",
  subagent: "Helpers",
  memory: "Memory & learning",
  title: "Chat titles",
  compaction: "Summaries",
  job: "Scheduled tasks",
  other: "Other",
};
const KIND_HELP: Record<string, string> = {
  chat: "Your chats, bots and channels",
  subagent: "Helpers an agent started for part of a task",
  memory: "Reviewing chats to remember what matters and learn skills",
  title: "Naming new chats",
  compaction: "Summing up long chats so they fit",
  job: "Prompts that ran on a schedule",
  other: "Everything else",
};

/** Named series in the chart; the rest fold into "Other". */
const MAX_SERIES = 7;

/** Midnight `n` days before today, local time. */
function daysAgo(n: number): number {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  d.setDate(d.getDate() - n);
  return d.getTime();
}
const periodFrom = (p: Period) => (p === "today" ? daysAgo(0) : p === "7d" ? daysAgo(6) : p === "30d" ? daysAgo(29) : 0);

const [pricing, setPricingSignal] = createSignal<Pricing>(lsGet<Pricing>("tessera.usage.pricing", "api") === "plans" ? "plans" : "api");
const setPricing = (v: Pricing) => {
  setPricingSignal(v);
  lsSet("tessera.usage.pricing", v);
};
/** The server knows API-equivalent costs and subscriptions (else: one cost, as billed). */
const [twoWays, setTwoWays] = createSignal(false);
/** The cost the page shows: at API prices (older servers: as billed), or billed per token. */
const costOf = (u: UsageTotals): number | undefined => (twoWays() && pricing() === "api" ? u.api_cost_usd ?? u.cost_usd : u.cost_usd);
/** A model's cost cell: "Free" for a local server nobody priced. */
const modelCost = (m: ModelUsage) => (m.billing === "local" && costOf(m.totals) == null ? "Free" : costText(m.totals));
const priceTip = (m: ModelUsage) => {
  if (!m.price) return m.billing === "local" ? "Runs on your own hardware" : costOf(m.totals) == null ? "No known price for this model" : undefined;
  const src = m.price_source === "custom" ? "your price" : m.price_source === "catalog" ? "public catalog" : "list price";
  return `$${m.price.input} in · $${m.price.output} out per million tokens (${src})`;
};

const tokensOf = (u: UsageTotals) => u.input + u.output + u.cache_read + u.cache_write;
const valueOf = (u: UsageTotals, m: Measure) => (m === "tokens" ? tokensOf(u) : m === "requests" ? u.requests : costOf(u) ?? 0);
const keyOf = (m: { provider: string; model: string }) => `${m.provider}/${m.model}`;
const nameOf = (m: { provider: string; model: string }) => modelLabel({ provider: m.provider, model: m.model });
const fmtInt = (n: number) => Math.round(n).toLocaleString();
/** Cost as text: unknown (no price) is a dash, not zero. */
const costText = (u: UsageTotals) => {
  const c = costOf(u);
  return c == null ? "—" : c > 0 ? fmtCost(c) ?? "$0" : "$0";
};
const fmtMeasure = (v: number, m: Measure) => (m === "cost" ? (v > 0 ? fmtCost(v) ?? "$0" : "$0") : m === "requests" ? fmtInt(v) : fmtTokens(v));

function dayKey(t: number): string {
  const d = new Date(t);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}
const parseDay = (k: string) => {
  const [y, m, d] = k.split("-").map(Number);
  return new Date(y, (m || 1) - 1, d || 1);
};
const fmtDay = (k: string, year = false) => parseDay(k).toLocaleDateString(undefined, { month: "short", day: "numeric", ...(year ? { year: "numeric" } : {}) });

// ── colours: one per model, kept while the app runs (never by rank) ──────

const SLOTS = 8;
const colorSlots = new Map<string, number>(lsGet<[string, number][]>("tessera.usage.colors", []));
function slotOf(key: string, keep: Set<string>): number {
  const have = colorSlots.get(key);
  if (have !== undefined) return have;
  const used = new Set([...colorSlots.entries()].filter(([k]) => keep.has(k)).map(([, s]) => s));
  let slot = 0;
  while (slot < SLOTS && used.has(slot)) slot++;
  if (slot === SLOTS) slot = colorSlots.size % SLOTS;
  // A slot freed by a model not on screen is reused.
  for (const [k, s] of colorSlots) if (s === slot && !keep.has(k)) colorSlots.delete(k);
  colorSlots.set(key, slot);
  lsSet("tessera.usage.colors", [...colorSlots.entries()]);
  return slot;
}

interface Series {
  key: string;
  label: string;
  color: string;
}

// ── the tab ──────────────────────────────────────────────────────────────

const [period, setPeriod] = createSignal<Period>("30d");
const [measure, setMeasure] = createSignal<Measure>("tokens");
const [stacked, setStacked] = createSignal(true);

export function UsageTab() {
  const [report, setReport] = createSignal<UsageReport | null>(null);
  const [error, setError] = createSignal<unknown>(null);
  const [loading, setLoading] = createSignal(false);
  /** The clock the limits' reset times and freshness are read against. */
  const [now, setNow] = createSignal(Date.now());
  const tick = setInterval(() => setNow(Date.now()), 30_000);
  onCleanup(() => clearInterval(tick));
  let gen = 0;
  const load = async () => {
    const g = ++gen;
    setLoading(true);
    try {
      // Days are this device's (its time zone, summer time included), not the server's.
      const from = periodFrom(period());
      const r = await call<UsageReport>({ method: "get_usage", params: { from, utc_offsets: utcOffsets(from) } });
      if (g !== gen) return;
      setTwoWays(Array.isArray(r.subscriptions));
      setNow(Date.now());
      setReport(r);
      setError(null);
    } catch (e) {
      if (g === gen) setError(e);
    } finally {
      if (g === gen) setLoading(false);
    }
  };
  createEffect(on(period, () => void load()));

  const r = () => report();
  const plans = () => twoWays() && pricing() === "plans";
  const subs = () => r()?.subscriptions ?? [];
  /** Subscription cards: always with plans; else for their limits (servers that report them). */
  const subCards = () => (plans() ? subs() : subs().filter((s) => Array.isArray(s.limits)));
  const isSub = (m: ModelUsage) => m.billing === "subscription" || (!m.billing && subs().some((s) => s.provider === m.provider));
  const anyCost = () => !!r() && r()!.by_model.some((m) => costOf(m.totals) != null);
  /** Models used without a price (the server's list when it sends one). */
  const unpriced = () => {
    const used = (r()?.by_model ?? []).filter((m) => m.totals.requests > 0);
    const list = r()?.unpriced;
    return list ? used.filter((m) => list.includes(keyOf(m))) : used.filter((m) => m.totals.cost_usd == null);
  };
  createEffect(() => {
    if (measure() === "cost" && r() && !anyCost()) setMeasure("tokens");
  });
  const oldServer = () => {
    const e = error();
    return e instanceof RpcError && (e.code === "bad_request" || e.code === "not_found") && /unknown|variant|method/i.test(e.message);
  };

  /** Days since the first request, for "All". */
  const allDays = () => {
    const first = r()?.by_day[0]?.day;
    return first ? Math.max(1, Math.round((daysAgo(0) - parseDay(first).getTime()) / 86_400_000) + 1) : 1;
  };
  const periodLabel = () =>
    period() === "today" ? "today" : period() === "all" ? `in the last ${plural(allDays(), "day", "days")}` : `in the last ${period() === "7d" ? 7 : 30} days`;
  /** A plan's share of the period: the server's (All: since the first request, not since 1970). */
  const planCost = (s: SubscriptionInfo) => {
    if (s.monthly_usd == null) return 0;
    const rep = r()!;
    const first = rep.by_day[0]?.day;
    if (period() === "all") return first ? (s.monthly_usd * (rep.to - parseDay(first).getTime())) / MONTH_MS : 0;
    return s.period_usd ?? (s.monthly_usd * (rep.to - rep.from)) / MONTH_MS;
  };
  const planTotal = () => subs().reduce((sum, s) => sum + planCost(s), 0);
  const costKpi = () => {
    const t = r()!.total;
    if (!twoWays())
      return { label: "Estimated cost", value: costText(t), sub: t.cost_usd == null ? (t.requests ? "No known prices" : undefined) : unpriced().length ? "Known prices only" : undefined };
    if (pricing() === "api")
      return { label: "At API prices", value: costText(t), sub: costOf(t) == null ? (t.requests ? "No known prices" : undefined) : unpriced().length ? "Known prices only" : "Estimated" };
    const billed = t.cost_usd ?? 0;
    const unset = subs().some((x) => x.monthly_usd == null);
    return {
      label: "You pay",
      value: money(planTotal() + billed),
      sub: !subs().length ? "Billed per token" : unset && !planTotal() ? "Add your plan above" : `${money(planTotal())} ${subs().filter((x) => x.monthly_usd != null).length > 1 ? "plans" : "plan"} + ${money(billed)} API`,
    };
  };

  return (
    <div class="stack usage" classList={{ refreshing: loading() && !!report() }}>
      <div class="usage-bar">
        <Segmented value={period()} options={PERIODS} onChange={setPeriod} label="Period" />
        <Show when={twoWays()}>
          <Segmented
            value={pricing()}
            options={[
              { value: "api", label: "API prices" },
              { value: "plans", label: "Subscription" },
            ]}
            onChange={setPricing}
            label="Pricing"
            class="usage-pricing"
          />
        </Show>
        <button type="button" class="icon-btn" aria-label="Refresh" data-tip="Refresh" disabled={loading()} onClick={() => void load()}>
          <Icon name="refresh" size={16} />
        </button>
      </div>

      <Show when={error() && !report()}>
        <div class="empty-state">
          <span class="empty-icon">
            <Icon name={oldServer() ? "server" : "alertCircle"} size={20} />
          </span>
          <p>{oldServer() ? "This server doesn't keep usage stats yet. Update Tessera on it to see them." : errorMessage(error())}</p>
        </div>
      </Show>

      <Show when={!report() && !error()}>
        <div class="empty-state">
          <span class="usage-spinner" aria-busy="true" />
        </div>
      </Show>

      <Show when={report()}>
        {(rep) => (
          <>
            <Show when={subCards().length} fallback={<Show when={plans()}>{<NoSubscriptions />}</Show>}>
              <div class="usage-subs">
                <For each={subCards()}>
                  {(s) => (
                    <SubscriptionCard
                      sub={s}
                      planCost={planCost(s)}
                      periodLabel={periodLabel()}
                      plans={plans()}
                      now={now()}
                      onSaved={() => void load()}
                    />
                  )}
                </For>
              </div>
            </Show>

            <div class="usage-kpis" role="group" aria-label={`Totals ${periodLabel()}`}>
              <Kpi label="Requests" value={fmtInt(rep().total.requests)} />
              <Kpi label="Tokens in" value={fmtTokens(rep().total.input)} exact={rep().total.input} />
              <Kpi label="Tokens out" value={fmtTokens(rep().total.output)} exact={rep().total.output} />
              <Kpi
                label="Cache read · write"
                value={`${fmtTokens(rep().total.cache_read)} · ${fmtTokens(rep().total.cache_write)}`}
                title={`${fmtInt(rep().total.cache_read)} read from the cache, ${fmtInt(rep().total.cache_write)} written to it`}
              />
              <Kpi label="All tokens" value={fmtTokens(tokensOf(rep().total))} exact={tokensOf(rep().total)} sub="In, out and cache" />
              <Kpi label={costKpi().label} value={costKpi().value} sub={costKpi().sub} />
            </div>

            <Show when={unpriced().length}>
              <UnpricedNote models={unpriced().map(keyOf)} onSaved={() => void load()} />
            </Show>

            <Show
              when={rep().total.requests > 0}
              fallback={
                <div class="empty-state">
                  <span class="empty-icon">
                    <Icon name="gauge" size={20} />
                  </span>
                  <p>No model requests {period() === "today" ? "today" : period() === "all" ? "yet" : "in this period"}.</p>
                </div>
              }
            >
              <Show when={period() !== "today"}>
                <div class="card usage-card">
                  <div class="usage-card-head">
                    <div class="setting-title grow">
                      {measure() === "tokens" ? "Tokens" : measure() === "requests" ? "Requests" : plans() ? "Billed per token" : twoWays() ? "Cost at API prices" : "Estimated cost"} per day
                    </div>
                    <Segmented
                      value={measure()}
                      options={[
                        { value: "tokens", label: "Tokens" },
                        { value: "requests", label: "Requests" },
                        ...(anyCost() ? [{ value: "cost" as const, label: "Cost" }] : []),
                      ]}
                      onChange={setMeasure}
                      label="Show"
                      class="sm"
                    />
                    <label class="usage-stack-toggle">
                      <span>By model</span>
                      <Toggle checked={stacked()} onChange={setStacked} label="Stack by model" />
                    </label>
                  </div>
                  <DayChart report={rep()} measure={measure()} stacked={stacked()} />
                </div>
              </Show>

              <div class="card usage-card">
                <div class="setting-title">By model</div>
                <div class="usage-table-wrap">
                  <table class="usage-table">
                    <thead>
                      <tr>
                        <th scope="col">Model</th>
                        <th scope="col">Requests</th>
                        <th scope="col">Input</th>
                        <th scope="col">Output</th>
                        <th scope="col">Cache read</th>
                        <th scope="col">Cache write</th>
                        <th scope="col">{costHead()}</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={rep().by_model}>
                        {(m) => (
                          <tr>
                            <th scope="row">
                              <span class="usage-model">
                                <span class="usage-model-name">{nameOf(m)}</span>
                                <span class="usage-model-provider">{m.provider}</span>
                              </span>
                            </th>
                            <td>{fmtInt(m.totals.requests)}</td>
                            <td title={fmtInt(m.totals.input)}>{fmtTokens(m.totals.input)}</td>
                            <td title={fmtInt(m.totals.output)}>{fmtTokens(m.totals.output)}</td>
                            <td title={fmtInt(m.totals.cache_read)}>{fmtTokens(m.totals.cache_read)}</td>
                            <td title={fmtInt(m.totals.cache_write)}>{fmtTokens(m.totals.cache_write)}</td>
                            <Show when={plans() && isSub(m)} fallback={<td title={priceTip(m)}>{modelCost(m)}</td>}>
                              <td class="usage-in-plan" title={m.totals.api_cost_usd != null ? `${money(m.totals.api_cost_usd)} at API prices` : undefined}>
                                In plan
                              </td>
                            </Show>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                </div>
              </div>

              <div class="card usage-card">
                <div class="setting-title">By kind</div>
                <KindBreakdown report={rep()} />
              </div>
            </Show>

            <p class="usage-note">
              <Icon name="info" size={13} />
              <span>
                <Show
                  when={plans()}
                  fallback={
                    <>
                      Costs are estimates from public pay-per-token list prices, for models whose price Tessera knows
                      {unpriced().length ? ` (not ${unpriced().map(nameOf).join(", ")})` : ""}.{twoWays() ? " For subscriptions they show what the same use would cost through the API." : ""}
                      {rep().by_model.some((m) => m.billing === "local") ? " Models on your own hardware are free." : ""} Your provider's bill is what counts.
                    </>
                  }
                >
                  A plan's monthly price is spread evenly over the period. Per-token costs are estimates from public list prices; your provider's bill is
                  what counts.
                </Show>
              </span>
            </p>
          </>
        )}
      </Show>
    </div>
  );
}

/** The cost column's name in the tables. */
const costHead = () => (!twoWays() ? "Est. cost" : pricing() === "api" ? "API price" : "Billed");

function Kpi(props: { label: string; value: string; exact?: number; title?: string; sub?: string }) {
  return (
    <div class="kpi" title={props.title ?? (props.exact !== undefined ? fmtInt(props.exact) : undefined)}>
      <div class="kpi-label">{props.label}</div>
      <div class="kpi-value">{props.value}</div>
      <Show when={props.sub}>
        <div class="kpi-sub">{props.sub}</div>
      </Show>
    </div>
  );
}

/** Each kind's share of the tokens, with its requests and cost. */
function KindBreakdown(props: { report: UsageReport }) {
  const rows = createMemo(() => [...props.report.by_kind].filter((k) => k.totals.requests > 0).sort((a, b) => tokensOf(b.totals) - tokensOf(a.totals)));
  const max = () => Math.max(1, ...rows().map((k) => tokensOf(k.totals)));
  const total = () => Math.max(1, tokensOf(props.report.total));
  return (
    <ul class="usage-kinds">
      <For each={rows()}>
        {(k) => (
          <li class="usage-kind" title={KIND_HELP[k.kind] ?? ""}>
            <div class="usage-kind-line">
              <span class="usage-kind-name">{KIND_LABEL[k.kind] ?? k.kind}</span>
              <span class="usage-kind-value">
                <strong>{fmtTokens(tokensOf(k.totals))}</strong> tokens · {plural(k.totals.requests, "request", "requests")}
                <Show when={k.totals.cost_usd != null}> · {costText(k.totals)}</Show>
              </span>
            </div>
            <div class="usage-kind-track" role="img" aria-label={`${Math.round((tokensOf(k.totals) / total()) * 100)}% of the tokens`}>
              <span class="usage-kind-fill" style={{ width: `${Math.max(0.6, (tokensOf(k.totals) / max()) * 100)}%` }} />
            </div>
          </li>
        )}
      </For>
    </ul>
  );
}

// ── the chart ────────────────────────────────────────────────────────────

interface Bucket {
  key: string;
  label: string;
  total: UsageTotals;
  /** Measure per series key. */
  values: Record<string, number>;
}

const ZERO: UsageTotals = { requests: 0, input: 0, output: 0, cache_read: 0, cache_write: 0 };
function addTotals(a: UsageTotals, b: UsageTotals): UsageTotals {
  const sum = (x: number | undefined, y: number | undefined) => (x == null && y == null ? undefined : (x ?? 0) + (y ?? 0));
  const cost = sum(a.cost_usd, b.cost_usd);
  const api = sum(a.api_cost_usd, b.api_cost_usd);
  return {
    requests: a.requests + b.requests,
    input: a.input + b.input,
    output: a.output + b.output,
    cache_read: a.cache_read + b.cache_read,
    cache_write: a.cache_write + b.cache_write,
    ...(cost === undefined ? {} : { cost_usd: cost }),
    ...(api === undefined ? {} : { api_cost_usd: api }),
  };
}

/** 0 and three or four round steps up to at least `max`. */
function niceTicks(max: number): number[] {
  if (max <= 0) return [0, 1];
  const raw = max / 4;
  const p = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * p).find((s) => s >= raw) ?? 10 * p;
  const out: number[] = [];
  for (let v = 0; v < max + step * 0.999; v += step) out.push(+v.toPrecision(12));
  return out;
}

function DayChart(props: { report: UsageReport; measure: Measure; stacked: boolean }) {
  let wrap!: HTMLDivElement;
  const [width, setWidth] = createSignal(600);
  /** The column under the mouse: a quick look beside it. */
  const [hover, setHover] = createSignal<number | null>(null);
  /** The column tapped, clicked or reached with the arrow keys: its details stay open under the chart. */
  const [picked, setPicked] = createSignal<number | null>(null);
  onMount(() => {
    const ro = new ResizeObserver(() => setWidth(Math.max(240, wrap.clientWidth)));
    ro.observe(wrap);
    setWidth(Math.max(240, wrap.clientWidth));
    // A tap or click anywhere else puts the details away (scrolling makes no click, so it keeps them).
    const away = (e: MouseEvent) => {
      if (picked() === null || !(e.target instanceof Element)) return;
      if (e.target.closest(".usage-hit, .usage-detail, .usage-card-head")) return;
      setPicked(null);
    };
    document.addEventListener("click", away);
    onCleanup(() => {
      ro.disconnect();
      document.removeEventListener("click", away);
    });
  });

  /** The named series (biggest first) and "Other". */
  const series = createMemo<Series[]>(() => {
    if (!props.stacked) return [{ key: "all", label: "All models", color: "var(--series-1)" }];
    const ranked = [...props.report.by_model].sort((a, b) => valueOf(b.totals, props.measure) - valueOf(a.totals, props.measure) || tokensOf(b.totals) - tokensOf(a.totals));
    const named = ranked.slice(0, ranked.length > MAX_SERIES + 1 ? MAX_SERIES : MAX_SERIES + 1);
    const keep = new Set(named.map(keyOf));
    const out: Series[] = named.map((m) => ({ key: keyOf(m), label: nameOf(m), color: `var(--series-${slotOf(keyOf(m), keep) + 1})` }));
    if (ranked.length > named.length) out.push({ key: "other", label: `${ranked.length - named.length} other models`, color: "var(--series-other)" });
    return out;
  });

  /** Days from the start of the period (or of the history) to today; weeks or months when there are many. */
  const grouped = createMemo<{ unit: "day" | "week" | "month"; list: Bucket[] }>(() => {
    const rep = props.report;
    const named = new Set(series().map((s) => s.key));
    const byDay = new Map(rep.by_day.map((d) => [d.day, d]));
    const first = rep.by_day.length ? rep.by_day[0].day : dayKey(rep.to);
    const start = props.report.from > 0 ? dayKey(rep.from) : first;
    const days: string[] = [];
    for (let d = parseDay(start < first ? start : first); dayKey(d.getTime()) <= dayKey(rep.to); d.setDate(d.getDate() + 1)) days.push(dayKey(d.getTime()));
    for (const k of byDay.keys()) if (!days.includes(k)) days.push(k);
    days.sort();
    const unit = days.length > 26 * 7 ? "month" : days.length > 92 ? "week" : "day";
    const groupOf = (k: string) => {
      if (unit === "day") return k;
      const d = parseDay(k);
      if (unit === "month") return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-01`;
      d.setDate(d.getDate() - ((d.getDay() + 6) % 7));
      return dayKey(d.getTime());
    };
    const out: Bucket[] = [];
    const year = days.length > 0 && parseDay(days[0]).getFullYear() !== new Date().getFullYear();
    for (const k of days) {
      const g = groupOf(k);
      let b = out[out.length - 1];
      if (!b || b.key !== g) {
        const label =
          unit === "day" ? fmtDay(g, year) : unit === "week" ? `Week of ${fmtDay(g, year)}` : parseDay(g).toLocaleDateString(undefined, { month: "long", year: "numeric" });
        b = { key: g, label, total: ZERO, values: {} };
        out.push(b);
      }
      const d = byDay.get(k);
      if (!d) continue;
      b.total = addTotals(b.total, d.totals);
      if (!props.stacked) {
        b.values.all = (b.values.all ?? 0) + valueOf(d.totals, props.measure);
        continue;
      }
      for (const m of d.models) {
        const key = named.has(keyOf(m)) ? keyOf(m) : "other";
        b.values[key] = (b.values[key] ?? 0) + valueOf(m.totals, props.measure);
      }
    }
    return { unit, list: out };
  });
  const buckets = () => grouped().list;
  const unitLabel = () => grouped().unit;
  // Other columns (another period, or days grouped into weeks): nothing picked.
  createEffect(
    on(
      () => buckets().map((b) => b.key).join(),
      () => {
        setPicked(null);
        setHover(null);
      },
      { defer: true },
    ),
  );
  const totalOf = (b: Bucket) => series().reduce((s, x) => s + (b.values[x.key] ?? 0), 0);
  const ticks = createMemo(() => niceTicks(Math.max(0, ...buckets().map(totalOf))));
  const top = () => ticks()[ticks().length - 1] || 1;

  const H = 190;
  const TIP_W = 200;
  const PAD_T = 8;
  const PAD_B = 22;
  const axisW = () => Math.max(28, ...ticks().map((t) => fmtMeasure(t, props.measure).length * 6.4 + 8));
  const plotW = () => width() - axisW() - 4;
  const band = () => plotW() / Math.max(1, buckets().length);
  const barW = () => Math.max(2, Math.min(24, band() * 0.72));
  const y = (v: number) => PAD_T + (H - PAD_T - PAD_B) * (1 - v / top());
  const x = (i: number) => axisW() + band() * i + (band() - barW()) / 2;

  /** Bars grow from the baseline; only the top segment has round corners; 2px gaps between segments. */
  const segPath = (x0: number, y0: number, w: number, h: number, round: boolean) => {
    if (!round || h < 1) return `M${x0},${y0}h${w}v${h}h${-w}z`;
    const r = Math.min(4, w / 2, h);
    return `M${x0},${y0 + h}V${y0 + r}Q${x0},${y0} ${x0 + r},${y0}H${x0 + w - r}Q${x0 + w},${y0} ${x0 + w},${y0 + r}V${y0 + h}z`;
  };
  const segments = createMemo(() =>
    buckets().map((b, i) => {
      const segs: { d: string; color: string }[] = [];
      let acc = 0;
      const present = series().filter((s) => (b.values[s.key] ?? 0) > 0);
      present.forEach((s, j) => {
        const v = b.values[s.key];
        const y1 = y(acc);
        const y0 = y(acc + v);
        acc += v;
        const isTop = j === present.length - 1;
        // The gap is cut from the top of every segment under another one.
        const h = y1 - y0 - (isTop ? 0 : 2);
        if (h <= 0.4) return;
        segs.push({ d: segPath(x(i), y0 + (isTop ? 0 : 2), barW(), h, isTop), color: s.color });
      });
      return segs;
    }),
  );

  const labelIdx = createMemo(() => {
    const n = buckets().length;
    if (n <= 1) return [0];
    const want = Math.max(2, Math.min(6, Math.floor(plotW() / 90)));
    const out = new Set<number>();
    for (let k = 0; k < want; k++) out.add(Math.round((k * (n - 1)) / (want - 1)));
    return [...out];
  });

  /** The column in focus: under the mouse, else the picked one. The others are quieter. */
  const lit = () => hover() ?? picked();
  const pick = (i: number) => {
    setPicked(i);
    setHover(null);
  };
  const onKey = (e: KeyboardEvent) => {
    const n = buckets().length;
    if (e.key === "Escape" && picked() !== null) {
      // Only the details close (the dialog stays).
      e.preventDefault();
      setPicked(null);
      return;
    }
    const cur = picked() ?? n;
    const to = e.key === "ArrowLeft" ? cur - 1 : e.key === "ArrowRight" ? cur + 1 : e.key === "Home" ? 0 : e.key === "End" ? n - 1 : null;
    if (to === null || !n) return;
    e.preventDefault();
    pick(Math.max(0, Math.min(n - 1, to)));
  };

  /** A column's models with something in it, top segment first. */
  const rowsOf = (b: Bucket) =>
    series()
      .filter((s) => (b.values[s.key] ?? 0) > 0)
      .reverse();
  /** The quick look beside the column under the mouse (not over the picked one: its details are open). */
  const tip = () => {
    const i = hover();
    if (i === null || i === picked() || !buckets()[i]) return null;
    const b = buckets()[i];
    // Beside the column, on whichever side has room; never over it.
    const right = x(i) + barW() + 10;
    const left = right + TIP_W <= width() ? right : Math.max(0, x(i) - 10 - TIP_W);
    return { b, rows: rowsOf(b), left };
  };
  /** The picked column's details (biggest model first), with the notch under it. */
  const detail = () => {
    const i = picked();
    const b = i === null ? undefined : buckets()[i];
    if (i === null || !b) return null;
    const rows = rowsOf(b).sort((p, q) => b.values[q.key] - b.values[p.key]);
    return { b, rows, notch: Math.max(14, Math.min(width() - 14, x(i) + barW() / 2)) };
  };
  const measureWord = () => (props.measure === "tokens" ? "tokens" : props.measure === "requests" ? "requests" : "estimated");

  return (
    <div class="usage-viz">
      <Show when={series().length > 1}>
        <ul class="usage-legend" aria-label="Models">
          <For each={series()}>
            {(s) => (
              <li>
                <span class="usage-swatch" style={{ background: s.color }} />
                {s.label}
              </li>
            )}
          </For>
        </ul>
      </Show>
      <div class="usage-plot" ref={wrap} onPointerLeave={() => setHover(null)}>
        <svg
          width={width()}
          height={H}
          viewBox={`0 0 ${width()} ${H}`}
          role="group"
          tabindex="0"
          aria-label={`${props.measure === "cost" ? "Estimated cost" : props.measure === "tokens" ? "Tokens" : "Requests"} per ${unitLabel()}. Left and right arrows show each ${unitLabel()}'s details.`}
          onKeyDown={onKey}
          onFocus={(e) => {
            // From the keyboard: the latest column, so the arrows have somewhere to start.
            if (picked() === null && e.currentTarget.matches(":focus-visible")) pick(buckets().length - 1);
          }}
        >
          <For each={ticks()}>
            {(t) => (
              <>
                <line class={t === 0 ? "usage-base" : "usage-grid"} x1={axisW()} x2={width()} y1={y(t)} y2={y(t)} />
                <text class="usage-tick" x={axisW() - 6} y={y(t)} dy="0.32em" text-anchor="end">
                  {fmtMeasure(t, props.measure)}
                </text>
              </>
            )}
          </For>
          <Show when={picked() !== null && buckets()[picked()!]}>
            <rect class="usage-picked" x={axisW() + band() * picked()!} y={0} width={band()} height={H - PAD_B} rx={4} />
          </Show>
          <For each={segments()}>
            {(segs, i) => (
              <g class="usage-col" classList={{ dim: lit() !== null && lit() !== i() }}>
                <For each={segs}>{(s) => <path d={s.d} style={{ fill: s.color }} />}</For>
              </g>
            )}
          </For>
          <For each={labelIdx()}>
            {(i) => (
              <text class="usage-tick" x={x(i) + barW() / 2} y={H - 6} text-anchor={i === 0 && buckets().length > 1 ? "start" : i === buckets().length - 1 && buckets().length > 1 ? "end" : "middle"}>
                {buckets()[i]?.label.replace(/^Week of /, "")}
              </text>
            )}
          </For>
          <For each={buckets()}>
            {(b, i) => (
              <rect
                class="usage-hit"
                data-day={b.key}
                x={axisW() + band() * i()}
                y={0}
                width={band()}
                height={H - PAD_B}
                onPointerMove={(e) => e.pointerType !== "touch" && setHover(i())}
                onClick={() => pick(i())}
              />
            )}
          </For>
        </svg>
        <Show when={tip()}>
          {(t) => (
            <div class="usage-tip" style={{ left: `${t().left}px` }} role="status">
              <div class="usage-tip-head">{t().b.label}</div>
              <div class="usage-tip-total">
                <strong>{fmtMeasure(totalOf(t().b), props.measure)}</strong>
                <span>{measureWord()}</span>
              </div>
              <For each={t().rows}>
                {(s) => (
                  <div class="usage-tip-row">
                    <span class="usage-key" style={{ background: s.color }} />
                    <strong>{fmtMeasure(t().b.values[s.key], props.measure)}</strong>
                    <span class="usage-tip-name">{s.label}</span>
                  </div>
                )}
              </For>
              <Show when={!t().rows.length}>
                <div class="usage-tip-row muted">Nothing</div>
              </Show>
              <div class="usage-tip-hint">Click for details</div>
            </div>
          )}
        </Show>
      </div>
      <Show when={detail()}>
        {(d) => (
          <div class="usage-detail" role="region" aria-label={`Details for ${d().b.label}`} aria-live="polite" style={{ "--notch": `${d().notch}px` }}>
            <div class="usage-detail-head">
              <span class="usage-detail-day">{d().b.label}</span>
              <button type="button" class="icon-btn usage-detail-close" aria-label="Close the details" onClick={() => setPicked(null)}>
                <Icon name="x" size={14} />
              </button>
            </div>
            <dl class="usage-detail-stats">
              <div>
                <dt>Requests</dt>
                <dd>{fmtInt(d().b.total.requests)}</dd>
              </div>
              <div title={fmtInt(d().b.total.input)}>
                <dt>Tokens in</dt>
                <dd>{fmtTokens(d().b.total.input)}</dd>
              </div>
              <div title={fmtInt(d().b.total.output)}>
                <dt>Tokens out</dt>
                <dd>{fmtTokens(d().b.total.output)}</dd>
              </div>
              <div title={`${fmtInt(d().b.total.cache_read)} read from the cache, ${fmtInt(d().b.total.cache_write)} written to it`}>
                <dt>Cached</dt>
                <dd>{fmtTokens(d().b.total.cache_read + d().b.total.cache_write)}</dd>
              </div>
              <div>
                <dt>{costHead()}</dt>
                <dd>{costText(d().b.total)}</dd>
              </div>
            </dl>
            <Show when={props.stacked && d().rows.length}>
              <ul class="usage-detail-models" aria-label={`${props.measure === "cost" ? "Cost" : props.measure === "tokens" ? "Tokens" : "Requests"} by model`}>
                <For each={d().rows}>
                  {(s) => (
                    <li>
                      <span class="usage-key" style={{ background: s.color }} />
                      <span class="usage-detail-name">{s.label}</span>
                      <strong>{fmtMeasure(d().b.values[s.key], props.measure)}</strong>
                      <span class="usage-detail-share">{Math.round((d().b.values[s.key] / Math.max(1e-12, totalOf(d().b))) * 100)}%</span>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
            <Show when={!d().b.total.requests}>
              <p class="usage-detail-none">No model requests this {unitLabel()}.</p>
            </Show>
          </div>
        )}
      </Show>
      <details class="usage-days">
        <summary>Numbers per {unitLabel()}</summary>
        <div class="usage-table-wrap">
          <table class="usage-table">
            <thead>
              <tr>
                <th scope="col">{unitLabel() === "day" ? "Day" : unitLabel() === "week" ? "Week of" : "Month"}</th>
                <th scope="col">Requests</th>
                <th scope="col">Tokens</th>
                <th scope="col">{costHead()}</th>
              </tr>
            </thead>
            <tbody>
              <For each={[...buckets()].reverse().filter((b) => b.total.requests > 0)}>
                {(b) => (
                  <tr>
                    <th scope="row">{b.label.replace(/^Week of /, "")}</th>
                    <td>{fmtInt(b.total.requests)}</td>
                    <td title={fmtInt(tokensOf(b.total))}>{fmtTokens(tokensOf(b.total))}</td>
                    <td>{costText(b.total)}</td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </div>
      </details>
    </div>
  );
}
