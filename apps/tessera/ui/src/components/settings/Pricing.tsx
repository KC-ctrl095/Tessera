// Settings → Usage, pricing: what subscriptions save compared with paying
// per token (`set_subscription`), and prices for models Tessera has none
// for (`set_model_price`). The numbers come from the usage report.
import { For, Show, createSignal } from "solid-js";
import type { SubscriptionInfo } from "../../proto/SubscriptionInfo";
import { call, modelLabel, state } from "../../lib/store";
import { errorMessage, setSettingsTab, toast } from "../../lib/ui";
import { fmtCost } from "../../lib/util";
import { Icon, Spinner } from "../icons";
import { PlanLimits } from "./Limits";

/** An average month, as the server spreads a plan's fee over a period. */
export const MONTH_MS = 30.44 * 86_400_000;

/** Plans people commonly pay for, by provider family (monthly, US dollars). */
const PLANS: { match: RegExp; plans: { name: string; usd: number }[] }[] = [
  {
    match: /codex|openai|chatgpt/i,
    plans: [
      { name: "Plus", usd: 20 },
      { name: "Pro", usd: 200 },
      { name: "Business", usd: 30 },
    ],
  },
  {
    match: /claude|anthropic/i,
    plans: [
      { name: "Pro", usd: 20 },
      { name: "Max 5×", usd: 100 },
      { name: "Max 20×", usd: 200 },
    ],
  },
];
const plansFor = (s: SubscriptionInfo) => {
  const kind = state.providers.find((p) => p.id === s.provider)?.kind ?? "";
  return PLANS.find((p) => p.match.test(`${s.provider} ${s.label} ${kind}`))?.plans ?? [];
};

export const money = (usd: number) => (usd > 0 ? fmtCost(usd) ?? "$0" : "$0");
const perMonth = (usd: number) => `${usd >= 100 || Number.isInteger(usd) ? `$${Math.round(usd)}` : `$${usd.toFixed(2)}`}/month`;
const parseUsd = (v: string) => {
  const n = Number(v.replace(/[$,\s]/g, ""));
  return v.trim() && Number.isFinite(n) && n >= 0 ? n : null;
};

/** One subscription: its usage limits; with `plans`, also its plan and what the same use would cost per token. */
export function SubscriptionCard(props: { sub: SubscriptionInfo; planCost: number; periodLabel: string; plans: boolean; now: number; onSaved: () => void }) {
  const [editing, setEditing] = createSignal(false);
  /** Typing a plan of one's own (always, when there are no common plans to pick). */
  const [custom, setCustom] = createSignal(false);
  const [name, setName] = createSignal("");
  const [price, setPrice] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const set = () => props.sub.monthly_usd != null;
  const planCost = () => props.planCost;
  const api = () => props.sub.api_cost_usd;
  const saved = () => (api() ?? 0) - planCost();
  const presets = () => plansFor(props.sub);
  const isPreset = () => presets().some((p) => p.name === props.sub.plan && p.usd === props.sub.monthly_usd);
  const open = () => {
    setName(props.sub.plan ?? "");
    setPrice(props.sub.monthly_usd != null ? String(props.sub.monthly_usd) : "");
    setError(null);
    setCustom(!isPreset());
    setEditing(true);
  };
  /** Both absent forgets the plan. */
  const save = async (plan?: string, monthly_usd?: number) => {
    setBusy(true);
    setError(null);
    try {
      await call({ method: "set_subscription", params: { provider: props.sub.provider, plan, monthly_usd } });
      setEditing(false);
      setCustom(false);
      props.onSaved();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const submit = (e: Event) => {
    e.preventDefault();
    const usd = parseUsd(price());
    if (usd === null) return setError("Enter what the plan costs per month, like 20.");
    void save(name().trim() || undefined, usd);
  };
  /** Bars on one scale: the plan's share of the period, and the same use per token. */
  const top = () => Math.max(planCost(), api() ?? 0, 1e-9);

  return (
    <div class="card usage-card usage-sub">
      <div class="usage-sub-head">
        <span class="usage-sub-icon">
          <Icon name="sparkles" size={15} />
        </span>
        <div class="grow">
          <div class="setting-title">{props.sub.label}</div>
          <Show when={props.plans || set()}>
            <div class="setting-help">
              <Show when={set()} fallback="Which plan do you pay for?">
                {props.sub.plan ? `${props.sub.plan} · ` : ""}
                {perMonth(props.sub.monthly_usd!)}
              </Show>
            </div>
          </Show>
        </div>
        <Show when={props.plans && set() && !editing()}>
          <button type="button" class="btn btn-sm btn-ghost" onClick={open}>
            Change
          </button>
        </Show>
      </div>

      <PlanLimits limits={props.sub.limits} now={props.now} />

      <Show when={props.plans && (!set() || editing())}>
        <div class="usage-plans" role="group" aria-label={`${props.sub.label} plans`}>
          <For each={presets()}>
            {(p) => (
              <button
                type="button"
                class="usage-plan"
                classList={{ on: props.sub.plan === p.name && props.sub.monthly_usd === p.usd }}
                disabled={busy()}
                onClick={() => void save(p.name, p.usd)}
              >
                <span class="usage-plan-name">{p.name}</span>
                <span class="usage-plan-price">{perMonth(p.usd)}</span>
              </button>
            )}
          </For>
          <Show when={presets().length}>
            <button type="button" class="usage-plan" classList={{ on: custom() }} aria-expanded={custom()} onClick={() => setCustom(!custom())}>
              <span class="usage-plan-name">Other…</span>
            </button>
          </Show>
          <Show when={editing() && !custom()}>
            <button type="button" class="usage-plan ghost" onClick={() => setEditing(false)}>
              <span class="usage-plan-name">Cancel</span>
            </button>
          </Show>
        </div>
        <Show when={custom() || !presets().length}>
        <form class="usage-plan-form" onSubmit={submit}>
          <label class="field">
            <span class="field-label">Plan</span>
            <input class="input" value={name()} placeholder="Name it" maxLength={40} onInput={(e) => setName(e.currentTarget.value)} />
          </label>
          <label class="field">
            <span class="field-label">Per month</span>
            <span class="usage-usd">
              <span aria-hidden="true">$</span>
              <input class="input" inputmode="decimal" value={price()} placeholder="20" aria-label="Monthly price in US dollars" onInput={(e) => setPrice(e.currentTarget.value)} />
            </span>
          </label>
          <div class="usage-plan-actions">
            <Show when={editing()}>
              <button type="button" class="btn btn-ghost" onClick={() => setEditing(false)}>
                Cancel
              </button>
            </Show>
            <button type="submit" class="btn" disabled={busy()}>
              <Show when={busy()}>
                <Spinner size={13} />
              </Show>
              Save
            </button>
          </div>
        </form>
        </Show>
        <Show when={editing() && set()}>
          <button type="button" class="link-btn usage-plan-clear" disabled={busy()} onClick={() => void save()}>
            Forget this plan
          </button>
        </Show>
        <Show when={error()}>
          <div class="form-error">{error()}</div>
        </Show>
      </Show>

      <Show when={props.plans && set() && !editing()}>
        <Show
          when={props.sub.requests > 0}
          fallback={<p class="setting-help usage-sub-empty">Not used {props.periodLabel}.</p>}
        >
          <div class="usage-compare">
            <div class="usage-compare-row">
              <span class="usage-compare-label">Your plan</span>
              <span class="usage-compare-track">
                <span class="usage-compare-fill plan" style={{ width: `${Math.max(0.8, (planCost() / top()) * 100)}%` }} />
              </span>
              <span class="usage-compare-value">{money(planCost())}</span>
            </div>
            <div class="usage-compare-row">
              <span class="usage-compare-label">At API prices</span>
              <span class="usage-compare-track">
                <span class="usage-compare-fill api" style={{ width: `${Math.max(0.8, ((api() ?? 0) / top()) * 100)}%` }} />
              </span>
              <span class="usage-compare-value">{api() == null ? "—" : money(api()!)}</span>
            </div>
          </div>
          <p class="usage-verdict" classList={{ good: saved() >= 0 }}>
            <Show
              when={api() != null}
              fallback={<>Tessera doesn't know the per-token prices of these models, so it can't compare.</>}
            >
              <Show when={saved() >= 0} fallback={<>Paying per token would have cost {money(-saved())} less {props.periodLabel}.</>}>
                <Icon name="check" size={14} stroke={2.4} />
                You saved {money(saved())} {props.periodLabel} with this plan
                <Show when={planCost() > 0 && api()! >= planCost() * 1.5}> ({(api()! / planCost()).toFixed(1)}× its price)</Show>.
              </Show>
            </Show>
          </p>
        </Show>
      </Show>
    </div>
  );
}

/** No subscriptions on this server: what the comparison needs, in plain words. */
export function NoSubscriptions() {
  return (
    <div class="card usage-card usage-sub">
      <div class="usage-sub-head">
        <span class="usage-sub-icon">
          <Icon name="sparkles" size={15} />
        </span>
        <div class="grow">
          <div class="setting-title">No subscriptions in use</div>
          <div class="setting-help">
            Sign in with ChatGPT in{" "}
            <button type="button" class="link-btn" onClick={() => setSettingsTab("models")}>
              Models
            </button>
            , or use your Claude Pro or Max plan through Claude Code, and this page shows its 5-hour and weekly limits and compares the plan with what the same use would cost per token.
          </div>
        </div>
      </div>
    </div>
  );
}

/** Models without a known price: say so, and let the user set one. */
export function UnpricedNote(props: { models: string[]; onSaved: () => void }) {
  const [open, setOpen] = createSignal(false);
  const name = (ref: string) => {
    const i = ref.indexOf("/");
    return i > 0 ? modelLabel({ provider: ref.slice(0, i), model: ref.slice(i + 1) }) : ref;
  };
  return (
    <div class="usage-unpriced" classList={{ open: open() }}>
      <div class="usage-unpriced-line">
        <Icon name="info" size={14} />
        <span>
          No price known for {props.models.map(name).join(", ")}, so {props.models.length === 1 ? "it isn't" : "they aren't"} in the costs.{" "}
          <button type="button" class="link-btn" aria-expanded={open()} onClick={() => setOpen(!open())}>
            {open() ? "Done" : props.models.length === 1 ? "Set a price" : "Set prices"}
          </button>
        </span>
      </div>
      <Show when={open()}>
        <ul class="usage-prices">
          <For each={props.models}>{(m) => <PriceRow model={m} label={name(m)} onSaved={props.onSaved} />}</For>
        </ul>
        <p class="setting-help usage-prices-help">US dollars per million tokens. Models on your own hardware cost nothing per token: “Free” counts them at $0.</p>
      </Show>
    </div>
  );
}

function PriceRow(props: { model: string; label: string; onSaved: () => void }) {
  const [input, setInput] = createSignal("");
  const [output, setOutput] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const i = props.model.indexOf("/");
  const provider = props.model.slice(0, i);
  const model = props.model.slice(i + 1);
  const save = async (inUsd: number, outUsd: number) => {
    setBusy(true);
    setError(null);
    try {
      await call({ method: "set_model_price", params: { provider, model, price: { input: inUsd, output: outUsd } } });
      toast({ level: "success", message: `Price saved for ${props.label}`, timeout: 1800 });
      props.onSaved();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const submit = (e: Event) => {
    e.preventDefault();
    const a = parseUsd(input());
    const b = parseUsd(output());
    if (a === null || b === null) return setError("Enter both prices, in dollars per million tokens (0 is fine).");
    void save(a, b);
  };
  return (
    <li class="usage-price">
      <form class="usage-price-form" onSubmit={submit}>
        <span class="usage-model">
          <span class="usage-model-name">{props.label}</span>
          <span class="usage-model-provider">{provider}</span>
        </span>
        <label class="usage-usd" title="Input, per million tokens">
          <span aria-hidden="true">$</span>
          <input class="input" inputmode="decimal" placeholder="Input" aria-label={`${props.label}: input price per million tokens`} value={input()} onInput={(e) => setInput(e.currentTarget.value)} />
        </label>
        <label class="usage-usd" title="Output, per million tokens">
          <span aria-hidden="true">$</span>
          <input class="input" inputmode="decimal" placeholder="Output" aria-label={`${props.label}: output price per million tokens`} value={output()} onInput={(e) => setOutput(e.currentTarget.value)} />
        </label>
        <button type="submit" class="btn btn-sm" disabled={busy()}>
          <Show when={busy()}>
            <Spinner size={12} />
          </Show>
          Save
        </button>
        <button type="button" class="btn btn-sm btn-ghost" disabled={busy()} onClick={() => void save(0, 0)}>
          Free
        </button>
      </form>
      <Show when={error()}>
        <div class="form-error">{error()}</div>
      </Show>
    </li>
  );
}
