import { For, Index, Match, Show, Switch, createMemo, createSignal, onMount } from "solid-js";
import { createStore } from "solid-js/store";
import "../styles/setup.css";
import { forgetServerEntry, isLocalEndpoint, pairAndConnect, pairLinkAndConnect, saveServer, setPhase } from "../lib/app";
import { backend, webAccess } from "../lib/backend";
import { suggestedName } from "../lib/devices";
import { isDesktopApp } from "../lib/native";
import { INSTALL_COMMAND } from "../lib/repo";
import { parsePairLink, type PairLinkInfo, type ProbeResult, type SavedServer } from "../lib/transport";
import { errorMessage, toast } from "../lib/ui";
import { defaultDeviceName, isTauriMobile } from "../lib/util";
import { Icon, Logo, Spinner } from "./icons";
import { MosaicArt } from "./Mosaic";

const isPairLink = (s: string) => /^\s*tessera:\/\/pair\?/.test(s);

interface HeaderRow {
  key: string;
  value: string;
  show: boolean;
}

export function normalizeEndpoint(s: string): string | null {
  let v = s.trim();
  if (!v) return null;
  if (!/^https?:\/\//i.test(v)) v = (/^(localhost|127\.|10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.|[\w-]+\.local\b|\[?::1)/.test(v) ? "http://" : "https://") + v;
  try {
    const u = new URL(v);
    return (u.origin + u.pathname).replace(/\/+$/, "");
  } catch {
    return null;
  }
}

/** The shared model behind the server fields: home + remote addresses, extras, headers. */
function useServerModel(initial?: SavedServer) {
  const eps = initial?.endpoints ?? [];
  const home = eps.find((e) => isLocalEndpoint(e)) ?? "";
  const remote = eps.find((e) => !isLocalEndpoint(e)) ?? "";
  const extra = eps.filter((e) => e !== home && e !== remote);
  const [m, setM] = createStore({
    name: initial?.name ?? "",
    home,
    remote,
    extra,
    headers: Object.entries(initial?.headers ?? {}).map(([key, value]): HeaderRow => ({ key, value, show: false })),
    advanced: extra.length > 0 || Object.keys(initial?.headers ?? {}).length > 0,
    probes: {} as Record<string, ProbeResult | "busy">,
  });
  const headerMap = () => {
    const out: Record<string, string> = {};
    for (const h of m.headers) if (h.key.trim()) out[h.key.trim()] = h.value;
    return out;
  };
  /** Endpoints in the order they are tried: home first. */
  const endpoints = (): { list: string[]; bad: boolean } => {
    const raw = [m.home, m.remote, ...m.extra].filter((x) => x.trim());
    const list = raw.map(normalizeEndpoint).filter((x): x is string => !!x);
    return { list: [...new Set(list)], bad: list.length !== raw.length };
  };
  const probe = async (key: string, value: string) => {
    const ep = normalizeEndpoint(value);
    if (!ep || !backend.probe) return;
    setM("probes", key, "busy");
    setM("probes", key, await backend.probe(ep, headerMap()));
  };
  /** Everything from a pairing link: addresses (home first), proxy headers, name. */
  const fill = (info: PairLinkInfo) => {
    const home = info.urls.find((u) => isLocalEndpoint(u)) ?? "";
    const remote = info.urls.find((u) => !isLocalEndpoint(u)) ?? "";
    const extra = info.urls.filter((u) => u !== home && u !== remote);
    setM({
      name: info.name || m.name,
      home,
      remote,
      extra,
      headers: Object.entries(info.headers).map(([key, value]): HeaderRow => ({ key, value, show: false })),
      advanced: extra.length > 0,
      probes: {},
    });
  };
  return { m, setM, headerMap, endpoints, probe, fill };
}

type Model = ReturnType<typeof useServerModel>;

function ProbeBadge(props: { p: ProbeResult | "busy" | undefined }) {
  return (
    <Show when={props.p}>
      {(p) => (
        <Show when={p() !== "busy"} fallback={<Spinner size={13} />}>
          <Show
            when={(p() as ProbeResult).ok}
            fallback={
              <span class="probe probe-bad" title={(p() as ProbeResult).error}>
                <Icon name="x" size={13} /> {(p() as ProbeResult).error}
              </span>
            }
          >
            <span class="probe probe-ok">
              <Icon name="check" size={13} /> Tessera {(p() as ProbeResult).version}
            </span>
          </Show>
        </Show>
      )}
    </Show>
  );
}

function AddressField(props: {
  model: Model;
  key: "home" | "remote";
  icon: "home" | "cloud";
  label: string;
  hint: string;
  placeholder: string;
  optional?: boolean;
  autofocus?: boolean;
  /** A whole pairing link pasted in here. */
  onLink?: (link: string) => void;
}) {
  const { m, setM, probe } = props.model;
  return (
    <div class="addr-field">
      <span class="addr-icon" classList={{ remote: props.icon === "cloud" }}>
        <Icon name={props.icon} size={16} />
      </span>
      <div class="addr-main">
        <label class="field-label" for={`addr-${props.key}`}>
          {props.label}
          <Show when={props.optional}>
            <span class="opt">optional</span>
          </Show>
        </label>
        <div class="addr-input">
          <input
            id={`addr-${props.key}`}
            class="input mono"
            type="url"
            inputmode="url"
            placeholder={props.placeholder}
            value={m[props.key]}
            autocomplete="off"
            autocapitalize="off"
            spellcheck={false}
            autofocus={props.autofocus}
            onInput={(e) => {
              const v = e.currentTarget.value;
              // A link pasted in whole (typed ones are picked up on Continue).
              if (props.onLink && isPairLink(v) && v.length - m[props.key].length > 20) {
                e.currentTarget.value = m[props.key];
                return props.onLink(v.trim());
              }
              setM(props.key, v);
              setM("probes", props.key, undefined!);
            }}
          />
          <Show when={backend.probe}>
            <button type="button" class="btn btn-sm btn-ghost" disabled={!m[props.key].trim()} onClick={() => probe(props.key, m[props.key])}>
              Test
            </button>
          </Show>
        </div>
        <div class="addr-foot">
          <span class="field-help">{props.hint}</span>
          <ProbeBadge p={m.probes[props.key]} />
        </div>
      </div>
    </div>
  );
}

/** Home + remote addresses, with extra endpoints and proxy headers under "Advanced". */
function ServerFields(props: { model: Model; autofocus?: boolean; showName?: boolean; onLink?: (link: string) => void }) {
  const { m, setM, probe } = props.model;
  return (
    <div class="server-fields">
      <AddressField
        model={props.model}
        key="home"
        icon="home"
        label="At home"
        hint="Your server's address on this network. Fastest; tried first."
        placeholder="192.168.1.10:7878"
        autofocus={props.autofocus}
        onLink={props.onLink}
      />
      <AddressField
        model={props.model}
        key="remote"
        icon="cloud"
        label="From anywhere"
        hint="A public URL for your server, e.g. through a Cloudflare Tunnel."
        placeholder="https://tessera.example.com"
        optional
        onLink={props.onLink}
      />
      <button type="button" class="disclosure" aria-expanded={m.advanced} onClick={() => setM("advanced", !m.advanced)}>
        <Icon name="chevronRight" size={14} class="disc-chev" />
        Advanced
        <Show when={m.extra.length + m.headers.length}>
          <span class="count">{m.extra.length + m.headers.length}</span>
        </Show>
      </button>
      <Show when={m.advanced}>
        <div class="advanced">
          <Show when={props.showName}>
            <label class="field">
              <span class="field-label">Name</span>
              <input class="input" placeholder="Home server" value={m.name} onInput={(e) => setM("name", e.currentTarget.value)} autocomplete="off" />
            </label>
          </Show>
          <div class="field">
            <span class="field-label">More addresses</span>
            <span class="field-help">Tried in order after the two above.</span>
            <Index each={m.extra}>
              {(ep, i) => (
                <div class="endpoint-row">
                  <input
                    class="input mono"
                    type="url"
                    inputmode="url"
                    placeholder="https://backup.example.com"
                    value={ep()}
                    aria-label={`Address ${i + 3}`}
                    autocomplete="off"
                    autocapitalize="off"
                    spellcheck={false}
                    onInput={(e) => setM("extra", i, e.currentTarget.value)}
                  />
                  <Show when={backend.probe}>
                    <button type="button" class="btn btn-sm btn-ghost" disabled={!ep().trim()} onClick={() => probe(`x${i}`, ep())}>
                      Test
                    </button>
                  </Show>
                  <button type="button" class="icon-btn" aria-label="Remove address" onClick={() => setM("extra", (l) => l.filter((_, j) => j !== i))}>
                    <Icon name="x" size={15} />
                  </button>
                </div>
              )}
            </Index>
            <button type="button" class="link-btn add-row" onClick={() => setM("extra", m.extra.length, "")}>
              <Icon name="plus" size={14} /> Add address
            </button>
          </div>
          <div class="field">
            <span class="field-label">Extra headers</span>
            <span class="field-help">
              Sent with every request — for example Cloudflare Access service tokens.
              <Show when={backend.kind === "browser"}> Browsers can't add headers to live connections; the apps can.</Show>
            </span>
            <For each={m.headers}>
              {(h, i) => (
                <div class="header-row">
                  <input class="input mono" placeholder="Header" value={h.key} aria-label="Header name" autocomplete="off" spellcheck={false} onInput={(e) => setM("headers", i(), "key", e.currentTarget.value)} />
                  <div class="secret">
                    <input
                      class="input mono"
                      type={h.show ? "text" : "password"}
                      placeholder="Value"
                      value={h.value}
                      aria-label="Header value"
                      autocomplete="off"
                      spellcheck={false}
                      onInput={(e) => setM("headers", i(), "value", e.currentTarget.value)}
                    />
                    <button type="button" class="icon-btn" aria-label={h.show ? "Hide value" : "Show value"} onClick={() => setM("headers", i(), "show", !h.show)}>
                      <Icon name={h.show ? "eyeOff" : "eye"} size={16} />
                    </button>
                  </div>
                  <button type="button" class="icon-btn" aria-label="Remove header" onClick={() => setM("headers", (l) => l.filter((_, j) => j !== i()))}>
                    <Icon name="x" size={15} />
                  </button>
                </div>
              )}
            </For>
            <div class="row gap wrap">
              <button type="button" class="link-btn add-row" onClick={() => setM("headers", m.headers.length, { key: "", value: "", show: true })}>
                <Icon name="plus" size={14} /> Add header
              </button>
              <button
                type="button"
                class="link-btn add-row"
                onClick={() =>
                  setM("headers", (l) => [
                    ...l.filter((x) => x.key.trim()),
                    { key: "CF-Access-Client-Id", value: "", show: true },
                    { key: "CF-Access-Client-Secret", value: "", show: false },
                  ])
                }
              >
                <Icon name="key" size={14} /> Cloudflare Access
              </button>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}

function CodeInput(props: { value: string; onInput: (v: string) => void; autofocus?: boolean }) {
  let el!: HTMLInputElement;
  onMount(() => props.autofocus && requestAnimationFrame(() => el.focus()));
  return (
    <input
      ref={el}
      class="input code-input"
      inputmode="numeric"
      autocomplete="one-time-code"
      placeholder="1234-5678"
      maxLength={12}
      value={props.value}
      aria-label="Pairing code"
      onInput={(e) => {
        // Keep digits; show 8-digit codes as 1234-5678 while typing.
        const d = e.currentTarget.value.replace(/\D/g, "").slice(0, 8);
        const v = d.length > 4 && d.length !== 6 ? `${d.slice(0, 4)}-${d.slice(4)}` : d;
        e.currentTarget.value = v;
        props.onInput(v);
      }}
    />
  );
}

const digits = (s: string) => s.replace(/\D/g, "");
/** 12345678 → 1234-5678, as the code field shows it. */
const prettyCode = (c: string) => {
  const d = digits(c);
  return d.length === 8 ? `${d.slice(0, 4)}-${d.slice(4)}` : d;
};

function PairHelp() {
  return (
    <span class="field-help">
      <Show when={!webAccess} fallback={<>Make one in <b>Settings → Devices</b> on a phone or computer where Tessera is already set up.</>}>
        On the server run <code>tessera pair</code>, or open <b>Settings → Devices</b> on a device that's already paired.
      </Show>
    </span>
  );
}

function FormError(props: { error: string | null; warn?: boolean; children?: import("solid-js").JSX.Element }) {
  return (
    <Show when={props.error}>
      <div class="form-error" classList={{ warn: !!props.warn }} role="alert">
        <Icon name={props.warn ? "shieldAlert" : "alertCircle"} size={16} />
        <div class="grow">
          <span>{props.error}</span>
          {props.children}
        </div>
      </div>
    </Show>
  );
}

/** A phone pointing at a QR code on a laptop. */
function ScanArt() {
  return (
    <svg class="route-art scan-art" viewBox="0 0 360 140" aria-hidden="true">
      <defs>
        <linearGradient id="sa-g" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stop-color="var(--accent)" />
          <stop offset="1" stop-color="var(--accent-2)" />
        </linearGradient>
      </defs>
      <g transform="translate(40 26)">
        <rect x="0" y="0" width="150" height="92" rx="9" class="ra-box" />
        <rect x="-12" y="96" width="174" height="7" rx="3.5" class="ra-box" />
        <g transform="translate(51 12)">
          <rect x="0" y="0" width="48" height="48" rx="4" fill="#fff" />
          <rect x="5" y="5" width="12" height="12" rx="2" fill="#111" />
          <rect x="31" y="5" width="12" height="12" rx="2" fill="#111" />
          <rect x="5" y="31" width="12" height="12" rx="2" fill="#111" />
          <rect x="22" y="22" width="5" height="5" fill="#111" />
          <rect x="31" y="31" width="5" height="5" fill="#111" />
          <rect x="38" y="36" width="5" height="7" fill="#111" />
          <rect x="22" y="33" width="5" height="10" fill="#111" />
        </g>
        <text x="75" y="80" class="ra-text">Settings → Devices</text>
      </g>
      <g transform="translate(236 14)">
        <rect x="0" y="0" width="74" height="118" rx="14" class="ra-box" />
        <rect x="7" y="12" width="60" height="88" rx="6" class="ra-screen" />
        <path d="M18 34v-8h8M48 26h8v8M56 76v8h-8M26 84h-8v-8" fill="none" stroke="url(#sa-g)" stroke-width="2.5" stroke-linecap="round" />
        <line x1="17" y1="55" x2="57" y2="55" stroke="url(#sa-g)" stroke-width="2" class="scan-line" />
      </g>
      <path d="M204 62 C 216 58, 222 58, 230 60" class="ra-line remote" />
    </svg>
  );
}

/**
 * This device's name for pairing: `kept` (pairing again keeps it), else the
 * best guess (the phone's model, the computer's name), until the user types.
 */
function deviceNameSignal(kept?: string): [() => string, (v: string) => void] {
  let typed = !!kept;
  const [name, setName] = createSignal(kept ?? defaultDeviceName());
  if (!typed) void suggestedName(defaultDeviceName()).then((n) => !typed && setName(n));
  return [
    name,
    (v: string) => {
      typed = true;
      setName(v);
    },
  ];
}

/**
 * Add/pair a server (modal, settings), or edit a saved one. `repair` pairs
 * an existing server again and replaces its entry on success.
 */
export function ServerForm(props: { mode: "pair" | "edit"; initial?: SavedServer; repair?: boolean; onDone?: () => void; onCancel?: () => void }) {
  const simple = backend.sameOrigin;
  const model = useServerModel(props.initial);
  const [code, setCode] = createSignal("");
  const [device, setDevice] = deviceNameSignal(props.initial?.deviceName);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // A pasted pairing link fills everything in. Left as it is, pairing goes
  // through the link, so the server's key is checked against it.
  let fromLink: { raw: string; info: PairLinkInfo } | null = null;
  const onLink = (raw: string) => {
    const info = parsePairLink(raw);
    if (typeof info === "string") return setError(info);
    setError(null);
    model.fill(info);
    setCode(prettyCode(info.code));
    fromLink = { raw, info };
  };
  const unchangedLink = (list: string[]): string | null => {
    if (!fromLink || !backend.pairLink) return null;
    const { raw, info } = fromLink;
    const same = (a: string[], b: string[]) => JSON.stringify([...a].sort()) === JSON.stringify([...b].sort());
    const urls = info.urls.map((u) => normalizeEndpoint(u) ?? u);
    const hdrs = (h: Record<string, string>) => Object.entries(h).map(([k, v]) => `${k}:${v}`);
    const ok = same(list, urls) && same(hdrs(model.headerMap()), hdrs(info.headers)) && digits(code()) === digits(info.code) && (!model.m.name.trim() || model.m.name.trim() === info.name);
    return ok ? raw : null;
  };

  const submit = async (e: Event) => {
    e.preventDefault();
    setError(null);
    const typed = [model.m.home, model.m.remote].find(isPairLink);
    if (props.mode === "pair" && typed) return onLink(typed.trim());
    const { list, bad } = simple ? { list: [location.origin], bad: false } : model.endpoints();
    if (!list.length) return setError("Add your server's address, e.g. 192.168.1.10:7878");
    if (bad) return setError("One of the addresses isn't a valid URL.");
    if (props.mode === "edit" && props.initial) {
      setBusy(true);
      try {
        await saveServer({ ...props.initial, name: model.m.name.trim() || props.initial.name, endpoints: list, headers: model.headerMap() });
        props.onDone?.();
      } catch (err) {
        setError(errorMessage(err));
      } finally {
        setBusy(false);
      }
      return;
    }
    if (!code().trim()) return setError("Enter the pairing code shown by your server.");
    setBusy(true);
    try {
      const link = unchangedLink(list);
      const deviceName = device().trim() || defaultDeviceName();
      const saved = link
        ? await pairLinkAndConnect(link, deviceName)
        : await pairAndConnect({ endpoints: list, headers: model.headerMap(), code: code().trim(), deviceName, name: model.m.name.trim() });
      if (props.repair && props.initial && props.initial.id !== saved.id) await forgetServerEntry(props.initial.id);
      toast({
        level: "success",
        title: `Paired with ${saved.name}`,
        message: saved.fingerprint ? `Server key ${saved.fingerprint} — it should match what \`tessera pair\` showed.` : "This device is ready.",
        timeout: 9000,
      });
      props.onDone?.();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form class="server-form" onSubmit={submit} novalidate>
      <Show when={!simple}>
        <ServerFields model={model} showName autofocus={props.mode === "pair" && !props.repair} onLink={props.mode === "pair" ? onLink : undefined} />
      </Show>
      <Show when={props.mode === "pair"}>
        <label class="field">
          <span class="field-label">Pairing code</span>
          <CodeInput value={code()} onInput={setCode} autofocus={simple || props.repair} />
          <PairHelp />
        </label>
        <label class="field">
          <span class="field-label">This device's name</span>
          <input class="input" value={device()} onInput={(e) => setDevice(e.currentTarget.value)} autocomplete="off" />
        </label>
      </Show>
      <FormError error={error()} />
      <div class="form-actions">
        <Show when={props.onCancel}>
          <button type="button" class="btn btn-ghost" onClick={props.onCancel}>
            Cancel
          </button>
        </Show>
        <button type="submit" class="btn btn-primary" disabled={busy()}>
          <Show when={busy()}>
            <Spinner size={15} />
          </Show>
          {props.mode === "edit" ? "Save" : busy() ? "Pairing…" : props.repair ? "Pair again" : "Pair & connect"}
        </button>
      </div>
    </form>
  );
}

/** LAN vs remote, drawn: this device reaches the server at home, or through a tunnel from anywhere. */
function RouteArt() {
  return (
    <svg class="route-art" viewBox="0 0 360 132" aria-hidden="true">
      <defs>
        <linearGradient id="ra-g" x1="0" y1="0" x2="1" y2="0">
          <stop offset="0" stop-color="var(--accent)" />
          <stop offset="1" stop-color="var(--accent-2)" />
        </linearGradient>
      </defs>
      {/* device */}
      <g transform="translate(18 40)">
        <rect x="0" y="6" width="68" height="44" rx="8" class="ra-box" />
        <rect x="8" y="14" width="52" height="28" rx="4" class="ra-screen" />
        <rect x="-6" y="52" width="80" height="5" rx="2.5" class="ra-box" />
      </g>
      {/* server */}
      <g transform="translate(276 30)">
        <rect x="0" y="0" width="66" height="30" rx="8" class="ra-box" />
        <rect x="0" y="36" width="66" height="30" rx="8" class="ra-box" />
        <circle cx="14" cy="15" r="3" class="ra-led" />
        <circle cx="14" cy="51" r="3" class="ra-led" />
        <rect x="26" y="13" width="28" height="4" rx="2" class="ra-slot" />
        <rect x="26" y="49" width="28" height="4" rx="2" class="ra-slot" />
      </g>
      {/* at home: short, solid */}
      <path d="M100 58 C 160 58, 210 46, 268 46" class="ra-line home" />
      <g transform="translate(172 38)">
        <rect x="-30" y="-12" width="60" height="20" rx="10" class="ra-tag" />
        <text x="0" y="2" class="ra-text">At home</text>
      </g>
      {/* anywhere: via the cloud, dashed */}
      <path d="M100 84 C 150 120, 230 122, 268 82" class="ra-line remote" />
      <g transform="translate(184 112)">
        <rect x="-36" y="-11" width="72" height="20" rx="10" class="ra-tag" />
        <text x="0" y="3" class="ra-text">Anywhere</text>
      </g>
    </svg>
  );
}

const STEPS = ["Server", "Pair", "Done"] as const;

/** How the first step starts: phones scan the QR code, the desktop app takes the pairing link, browsers type the address. */
type Way = "scan" | "link" | "manual";

/** A pasted pairing link, read back: which server, its addresses, and what comes along (never the secrets themselves). */
function LinkSummary(props: { info: PairLinkInfo; keyChecked: boolean; onClear: () => void }) {
  const names = () => Object.keys(props.info.headers);
  return (
    <div class="link-summary" aria-live="polite">
      <div class="ls-head">
        <Icon name="check" size={14} />
        <span class="grow">
          Pairing link for <b>{props.info.name || "your server"}</b>
        </span>
        <button type="button" class="link-btn" onClick={() => props.onClear()}>
          Paste another
        </button>
      </div>
      <ul class="ls-urls">
        <For each={props.info.urls}>
          {(u) => (
            <li>
              <Icon name={isLocalEndpoint(u) ? "home" : "cloud"} size={14} />
              <span class="mono">{u}</span>
              <span class="ls-where">{isLocalEndpoint(u) ? "at home" : "anywhere"}</span>
            </li>
          )}
        </For>
      </ul>
      <Show when={names().length}>
        <div class="ls-line">
          <Icon name="key" size={13} />
          {names().some((n) => /^cf-access-/i.test(n)) ? "Cloudflare Access keys included" : `Extra headers included (${names().join(", ")})`}
        </div>
      </Show>
      <Show when={props.keyChecked && props.info.fingerprint}>
        <div class="ls-line">
          <Icon name="lock" size={13} />
          <span>
            Server key <span class="mono">{props.info.fingerprint}</span> — checked as it pairs
          </span>
        </div>
      </Show>
    </div>
  );
}

/** First run: 1 · where the server is → 2 · pairing code → 3 · you're in. */
export function Onboarding(props: { onBack?: () => void } = {}) {
  const simple = backend.sameOrigin;
  const model = useServerModel();
  const [step, setStep] = createSignal(simple ? 1 : 0);
  const [code, setCode] = createSignal("");
  const [device, setDevice] = deviceNameSignal();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [paired, setPaired] = createSignal<SavedServer | null>(null);
  let failedProbe = "";
  // The phone app leads with scanning the QR code; the desktop app with the
  // link `tessera pair` prints (it carries the addresses and access keys).
  const [way, setWay] = createSignal<Way>(isTauriMobile ? "scan" : isDesktopApp ? "link" : "manual");
  const [link, setLink] = createSignal("");
  const linkInfo = createMemo(() => (link().trim() ? parsePairLink(link()) : null));
  const goodLink = () => {
    const i = linkInfo();
    return i && typeof i !== "string" ? i : null;
  };
  // A link pasted in whole is put away once read (it holds secrets); one being typed stays in view.
  const [tucked, setTucked] = createSignal(false);
  let pairBtn: HTMLButtonElement | undefined;
  const [warnError, setWarnError] = createSignal(false);
  /** The server's key was checked against what was scanned or pasted. */
  const [verified, setVerified] = createSignal<"QR code" | "pairing link" | null>(null);
  const [permDenied, setPermDenied] = createSignal(false);
  const [editName, setEditName] = createSignal(false);
  const goWay = (w: Way) => {
    setError(null);
    setWarnError(false);
    setWay(w);
  };
  const pastedLink = (l: string) => {
    setLink(l);
    setTucked(true);
    goWay("link");
  };

  /** Pair with everything from a scanned or pasted link; the apps check the server's key against it. */
  const pairWithLink = async (raw: string, from: "QR code" | "pairing link" = "pairing link") => {
    const info = parsePairLink(raw);
    if (typeof info === "string") return setError(info);
    setError(null);
    setWarnError(false);
    setBusy(true);
    try {
      const name = device().trim() || defaultDeviceName();
      const saved = backend.pairLink
        ? await pairLinkAndConnect(raw.trim(), name, { stay: true })
        : await pairAndConnect({ endpoints: info.urls, headers: info.headers, code: info.code, deviceName: name, name: info.name }, { stay: true });
      setPaired(saved);
      setVerified(backend.pairLink && info.fingerprint ? from : null);
      setStep(2);
    } catch (err) {
      const msg = errorMessage(err);
      // A key mismatch means something may be intercepting: say so loudly.
      setWarnError(/intercept|doesn.t match|does not match/i.test(msg));
      setError(msg);
    } finally {
      setBusy(false);
    }
  };

  const scanNow = async () => {
    setError(null);
    setWarnError(false);
    setPermDenied(false);
    let text: string | null;
    try {
      const bs = await import("@tauri-apps/plugin-barcode-scanner");
      let perm = await bs.checkPermissions();
      if (perm !== "granted") perm = await bs.requestPermissions();
      if (perm !== "granted") {
        setPermDenied(true);
        setError("Tessera needs the camera to scan the pairing code. Allow camera access for Tessera, or enter the details instead.");
        return;
      }
      const { scanQr } = await import("./Scanner");
      text = await scanQr();
    } catch (err) {
      const msg = errorMessage(err);
      if (!/cancel/i.test(msg)) setError(msg);
      return;
    }
    if (text === null) return; // backed out of the camera
    if (!isPairLink(text)) {
      setError("That isn't a Tessera pairing code. On the server run tessera pair (or open Settings → Devices → Get a code on a paired device) and scan the QR code shown there.");
      return;
    }
    await pairWithLink(text, "QR code");
  };
  const openCameraSettings = async () => {
    try {
      const bs = await import("@tauri-apps/plugin-barcode-scanner");
      await bs.openAppSettings();
    } catch {
      /* not available */
    }
  };

  const next = async (e: Event) => {
    e.preventDefault();
    setError(null);
    const typed = [model.m.home, model.m.remote].find(isPairLink);
    if (typed) return pastedLink(typed.trim());
    const { list, bad } = model.endpoints();
    if (!list.length) return setError("Enter your server's address — for example 192.168.1.10:7878.");
    if (bad) return setError("That address doesn't look right. Try something like 192.168.1.10:7878 or https://tessera.example.com.");
    if (backend.probe && !model.m.remote.trim() && failedProbe !== list[0]) {
      // Quick reachability check for the home address (browser only).
      setBusy(true);
      const p = await backend.probe(list[0], model.headerMap());
      setBusy(false);
      model.setM("probes", "home", p);
      if (!p.ok) {
        failedProbe = list[0];
        return setError(`Couldn't reach ${list[0]} (${p.error}). Check the address — or press Continue again to pair anyway.`);
      }
    }
    setStep(1);
  };

  const pair = async (e: Event) => {
    e.preventDefault();
    setError(null);
    if (!code().trim()) return setError("Enter the pairing code shown by your server.");
    setBusy(true);
    try {
      const saved = await pairAndConnect(
        {
          endpoints: simple ? [location.origin] : model.endpoints().list,
          headers: model.headerMap(),
          code: code().trim(),
          deviceName: device().trim() || defaultDeviceName(),
          name: model.m.name.trim(),
        },
        { stay: true },
      );
      setPaired(saved);
      setStep(2);
    } catch (err) {
      // Server explanations (e.g. "pairing is only allowed from…") are shown verbatim.
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  /** "This phone will appear as …", changeable in place. */
  const DeviceNameLine = () => (
    <div class="device-name-line">
      <Show
        when={editName()}
        fallback={
          <>
            <span class="muted small">
              This {isTauriMobile ? "device" : "computer"} will appear as <b>{device()}</b>
            </span>
            <button type="button" class="link-btn" onClick={() => setEditName(true)}>
              Change
            </button>
          </>
        }
      >
        <input class="input" value={device()} aria-label="This device's name" onInput={(e) => setDevice(e.currentTarget.value)} autocomplete="off" />
      </Show>
    </div>
  );
  const RunHereInstead = () => (
    <Show when={props.onBack}>
      <button type="button" class="btn btn-ghost btn-lg full" onClick={() => props.onBack?.()}>
        <Icon name="arrowLeft" size={16} /> Run Tessera on this computer instead
      </button>
    </Show>
  );

  return (
    <div class="onboarding">
      <MosaicArt class="onb-mosaic" seed={3} width={760} height={200} cell={15} focus={[0.5, 0.7]} />
      <div class="onb-card">
        <div class="onb-head">
          <Logo size={44} />
          <ol class="stepper" aria-label="Steps">
            <For each={STEPS}>
              {(s, i) => (
                <li classList={{ on: step() === i(), done: step() > i() }} aria-current={step() === i() ? "step" : undefined}>
                  <span class="step-dot">
                    <Show when={step() > i()} fallback={i() + 1}>
                      <Icon name="check" size={12} stroke={2} />
                    </Show>
                  </span>
                  <span class="step-label">{s}</span>
                </li>
              )}
            </For>
          </ol>
        </div>

        <Switch>
          <Match when={step() === 0 && way() === "scan"}>
            <div class="onb-body">
              <h1 class="serif">Pair with your server</h1>
              <p class="onb-lede">
                On the server run <code>tessera pair</code> — or on a computer that's already paired, open <b>Settings → Devices</b> and tap <b>Get a code</b>.
                Then scan the QR code: it brings the server's addresses and keys with it.
              </p>
              <ScanArt />
              <FormError error={error()} warn={warnError()}>
                <Show when={permDenied()}>
                  <button type="button" class="link-btn" onClick={() => void openCameraSettings()}>
                    Open app settings
                  </button>
                </Show>
              </FormError>
              <button type="button" class="btn btn-primary btn-xl full" disabled={busy()} onClick={() => void scanNow()}>
                <Show when={busy()} fallback={<Icon name="scan" size={18} />}>
                  <Spinner size={16} />
                </Show>
                {busy() ? "Pairing…" : "Scan QR code"}
              </button>
              <button type="button" class="btn btn-ghost btn-lg full" onClick={() => goWay("manual")}>
                Enter details instead
              </button>
              <DeviceNameLine />
            </div>
          </Match>
          <Match when={step() === 0 && way() === "link"}>
            <form
              class="onb-body"
              onSubmit={(e) => {
                e.preventDefault();
                void pairWithLink(link());
              }}
              novalidate
            >
              <h1 class="serif">Connect to your server</h1>
              <p class="onb-lede">
                On the server, <code>tessera setup</code> (or later <code>tessera pair</code>) shows a QR code and a pairing link. Copy the link and paste it
                here — it brings the addresses, access keys and a one-time code, so there's nothing to type.
              </p>
              <Show when={!(goodLink() && tucked())}>
                <label class="field">
                  <span class="field-label">Pairing link</span>
                  <textarea
                    class="input mono link-input"
                    rows={3}
                    placeholder="tessera://pair?…"
                    value={link()}
                    onInput={(e) => {
                      const v = e.currentTarget.value;
                      setError(null);
                      setTucked(v.length - link().length > 20);
                      setLink(v);
                      if (goodLink() && tucked()) requestAnimationFrame(() => pairBtn?.focus());
                    }}
                    autocomplete="off"
                    autocapitalize="off"
                    spellcheck={false}
                    ref={(el) => !isTauriMobile && requestAnimationFrame(() => el.focus())}
                  />
                  <span class="field-help">It's private — it lets a device in. Paste it here only.</span>
                </label>
              </Show>
              <Show when={goodLink()}>
                {(i) => (
                  <LinkSummary
                    info={i()}
                    keyChecked={!!backend.pairLink}
                    onClear={() => {
                      setLink("");
                      setTucked(false);
                      setError(null);
                    }}
                  />
                )}
              </Show>
              <Show when={typeof linkInfo() === "string"}>
                <div class="form-error warn">{linkInfo() as string}</div>
              </Show>
              <FormError error={error()} warn={warnError()} />
              <button ref={pairBtn} type="submit" class="btn btn-primary btn-xl full" disabled={busy() || !goodLink()}>
                <Show when={busy()} fallback={<Icon name="key" size={17} />}>
                  <Spinner size={16} />
                </Show>
                {busy() ? "Pairing…" : "Pair & connect"}
              </button>
              <DeviceNameLine />
              <button type="button" class="btn btn-ghost btn-lg full" onClick={() => goWay("manual")}>
                Enter the address and code instead
              </button>
              <Show when={isTauriMobile}>
                <button type="button" class="btn btn-ghost btn-lg full" onClick={() => goWay("scan")}>
                  <Icon name="scan" size={16} /> Scan a QR code instead
                </button>
              </Show>
              <RunHereInstead />
            </form>
          </Match>
          <Match when={step() === 0}>
            <form class="onb-body" onSubmit={next} novalidate>
              <h1 class="serif">Where is your Tessera server?</h1>
              <p class="onb-lede">Tessera runs on your own computer. Tell this app how to reach it — at home, and optionally from anywhere.</p>
              <Show when={props.onBack}>
                <ol class="onb-howto">
                  <li>
                    Install Tessera on that computer with one command: <code class="setup-cmd">{INSTALL_COMMAND}</code>
                  </li>
                  <li>Its setup ends with a pairing link: paste it here (or enter its address and code).</li>
                </ol>
              </Show>
              <RouteArt />
              <ServerFields model={model} autofocus={!isTauriMobile && !simple} showName onLink={pastedLink} />
              <FormError error={error()} />
              <button type="submit" class="btn btn-primary btn-xl full" disabled={busy()}>
                <Show when={busy()} fallback={<>Continue</>}>
                  <Spinner size={16} /> Checking…
                </Show>
                <Show when={!busy()}>
                  <Icon name="arrowRight" size={17} />
                </Show>
              </button>
              <Show
                when={isTauriMobile}
                fallback={
                  <button type="button" class="btn btn-ghost btn-lg full" onClick={() => goWay("link")}>
                    <Icon name="link" size={16} /> Paste a pairing link instead
                  </button>
                }
              >
                <button type="button" class="btn btn-ghost btn-lg full" onClick={() => goWay("scan")}>
                  <Icon name="scan" size={16} /> Scan a QR code instead
                </button>
              </Show>
              <RunHereInstead />
            </form>
          </Match>
          <Match when={step() === 1}>
            <form class="onb-body" onSubmit={pair} novalidate>
              <h1 class="serif">{simple ? "Pair this browser" : "Enter your pairing code"}</h1>
              <p class="onb-lede">
                {simple
                  ? `A one-time code lets this browser use ${location.host}. You only do this once.`
                  : "A one-time code proves this device is yours. It also pins your server's key, so nobody can impersonate it."}
              </p>
              <div class="pair-box">
                <CodeInput value={code()} onInput={setCode} autofocus />
                <PairHelp />
              </div>
              <label class="field">
                <span class="field-label">This device's name</span>
                <input class="input" value={device()} onInput={(e) => setDevice(e.currentTarget.value)} autocomplete="off" />
              </label>
              <FormError error={error()} />
              <div class="onb-actions">
                <Show when={!simple}>
                  <button type="button" class="btn btn-ghost btn-lg" onClick={() => { setError(null); setStep(0); }}>
                    <Icon name="arrowLeft" size={16} /> Back
                  </button>
                </Show>
                <button type="submit" class="btn btn-primary btn-xl grow" disabled={busy()}>
                  <Show when={busy()} fallback={<Icon name="key" size={17} />}>
                    <Spinner size={16} />
                  </Show>
                  {busy() ? "Pairing…" : "Pair & connect"}
                </button>
              </div>
            </form>
          </Match>
          <Match when={step() === 2}>
            <div class="onb-body onb-done">
              <div class="done-badge">
                <Icon name="check" size={30} stroke={2} />
              </div>
              <h1 class="serif">You're in</h1>
              <p class="onb-lede">
                {paired()?.name ? `This device is paired with ${paired()!.name}.` : "This device is paired."} Your chats and keys stay on your server.
              </p>
              <Show when={paired()?.fingerprint}>
                <div class="fingerprint">
                  <Icon name={verified() ? "shieldCheck" : "lock"} size={15} />
                  <div class="grow">
                    <div class="fp-label">
                      Server key
                      <Show when={verified()}>
                        <span class="badge badge-ok">
                          <Icon name="check" size={11} /> Verified
                        </span>
                      </Show>
                    </div>
                    <div class="fp-value mono">{paired()!.fingerprint}</div>
                    <div class="field-help">
                      <Show
                        when={verified()}
                        fallback={
                          <>
                            Matches <code>tessera pair</code> on your server — everything is end-to-end encrypted to this key.
                          </>
                        }
                      >
                        It matches the {verified()}, so nothing is in between. Everything is end-to-end encrypted to this key.
                      </Show>
                    </div>
                  </div>
                </div>
              </Show>
              <button type="button" class="btn btn-primary btn-xl full" onClick={() => setPhase("app")} autofocus>
                Open Tessera <Icon name="arrowRight" size={17} />
              </button>
            </div>
          </Match>
        </Switch>
      </div>
      <p class="onb-foot">
        <Icon name="shield" size={13} /> This device only keeps a revocable token. Remove it any time from Settings → Devices.
      </p>
    </div>
  );
}
