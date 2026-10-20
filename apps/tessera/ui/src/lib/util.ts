// Small, dependency-free helpers shared across the UI.

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** The Android / iOS app (camera, QR scanning). */
export const isTauriMobile = isTauri && typeof navigator !== "undefined" && /Android|iPhone|iPad/i.test(navigator.userAgent);

export const coarsePointer = () =>
  typeof matchMedia !== "undefined" && matchMedia("(pointer: coarse)").matches;

/**
 * A touch-first device (phone, foldable, tablet app): no keyboard-shortcut
 * hints anywhere (kbd chips, shortcut text in tooltips, the cheatsheet).
 */
export const touchUi = () => isTauriMobile || coarsePointer();

export const reducedMotion = () =>
  typeof matchMedia !== "undefined" && matchMedia("(prefers-reduced-motion: reduce)").matches;

export function uid(prefix = ""): string {
  const rnd =
    typeof crypto !== "undefined" && "getRandomValues" in crypto
      ? Array.from(crypto.getRandomValues(new Uint8Array(6)), (b) => b.toString(16).padStart(2, "0")).join("")
      : Math.random().toString(16).slice(2, 14);
  return prefix + rnd;
}

export const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

const TOKEN_UNITS = ["", "k", "M", "B", "T"];

/**
 * A token count in a few characters, at most three digits: 950, 1.23k, 42.3k, 262k, 1.5M, 15.3M, 1.5B.
 * Each unit is a thousand of the one before, and a number that rounds up to 1,000 of its unit is
 * shown in the next one ("1M", never "1000k"). The exact figure stays in the tooltips that offer it.
 */
export function fmtTokens(n: number): string {
  if (!Number.isFinite(n)) return "–";
  const sign = n < 0 ? "-" : "";
  const x = Math.round(Math.abs(n));
  if (x < 1000) return sign + x;
  // The unit whose count is 1 to 999, then 2, 1 or 0 decimals for three digits at most. Rounding is
  // done on whole numbers (x / 10 and the like), so a half rounds up the way it reads (1,005 → 1.01k).
  let u = 0;
  let scale = 1;
  while (x >= scale * 1000 && u < TOKEN_UNITS.length - 1) {
    scale *= 1000;
    u++;
  }
  const d = x < scale * 10 ? 2 : x < scale * 100 ? 1 : 0;
  let v = Math.round(x / (scale / 10 ** d)) / 10 ** d;
  if (v >= 1000 && u < TOKEN_UNITS.length - 1) {
    v = 1;
    u++;
  }
  return sign + String(Number(v.toFixed(d))) + TOKEN_UNITS[u];
}

export function fmtDuration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

export function relTime(ts: number, now = Date.now()): string {
  const d = now - ts;
  const future = d < 0;
  const a = Math.abs(d);
  const f = (v: number, u: string) => (future ? `in ${v}${u}` : `${v}${u} ago`);
  if (a < 45_000) return future ? "in a moment" : "just now";
  if (a < 3_600_000) return f(Math.round(a / 60_000), "m");
  if (a < 86_400_000) return f(Math.round(a / 3_600_000), "h");
  if (a < 7 * 86_400_000) return f(Math.round(a / 86_400_000), "d");
  return new Date(ts).toLocaleDateString(undefined, { month: "short", day: "numeric", year: sameYear(ts, now) ? undefined : "numeric" });
}
const sameYear = (a: number, b: number) => new Date(a).getFullYear() === new Date(b).getFullYear();

export function fmtDateTime(ts: number): string {
  return new Date(ts).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

export function basename(path: string): string {
  const p = path.replace(/[\\/]+$/, "");
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return i >= 0 ? p.slice(i + 1) || p : p || "/";
}

export function tildify(path: string, home: string | undefined): string {
  if (home && (path === home || path.startsWith(home + "/"))) return "~" + path.slice(home.length);
  return path;
}

export function lsGet<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw == null ? fallback : (JSON.parse(raw) as T);
  } catch {
    return fallback;
  }
}

export function lsSet(key: string, value: unknown): void {
  try {
    if (value === undefined) localStorage.removeItem(key);
    else localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* quota or private mode: ignore */
  }
}

export function debounce<A extends unknown[]>(fn: (...a: A) => void, ms: number) {
  let t: ReturnType<typeof setTimeout> | undefined;
  const d = (...a: A) => {
    clearTimeout(t);
    t = setTimeout(() => fn(...a), ms);
  };
  d.flush = (...a: A) => {
    clearTimeout(t);
    fn(...a);
  };
  d.cancel = () => clearTimeout(t);
  return d;
}

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.cssText = "position:fixed;opacity:0;pointer-events:none";
    document.body.appendChild(ta);
    ta.select();
    let ok = false;
    try {
      ok = document.execCommand("copy");
    } catch {
      ok = false;
    }
    ta.remove();
    return ok;
  }
}

/** Open a URL in the system browser (Tauri) or a new tab. */
export async function openExternal(url: string): Promise<void> {
  if (!/^(https?:|mailto:)/i.test(url)) return;
  if (isTauri) {
    // The opener is scoped to web and mail links; never fall back to opening
    // a window inside the app.
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("plugin:opener|open_url", { url });
    } catch (e) {
      console.warn("could not open link", e);
    }
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}

/** What kind of device this is, from the user agent (`suggestedName` in lib/devices.ts knows better). */
export function defaultDeviceName(): string {
  const ua = navigator.userAgent;
  // Not "tablet" without "Mobile": an unfolded foldable phone says the same.
  if (/Android/i.test(ua)) return /Mobile/i.test(ua) ? "Android phone" : "Android device";
  if (/iPhone|iPad/i.test(ua)) return "iOS device";
  const browser = /Firefox\//.test(ua) ? "Firefox" : /Edg\//.test(ua) ? "Edge" : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : "Browser";
  const os = /Windows/i.test(ua) ? "Windows" : /Mac OS/i.test(ua) ? "macOS" : /Linux/i.test(ua) ? "Linux" : "";
  if (isTauri) return os ? `${os} desktop` : "Desktop";
  return os ? `${browser} on ${os}` : browser;
}

export function greeting(d = new Date()): string {
  const h = d.getHours();
  if (h < 5) return "Up late";
  if (h < 12) return "Good morning";
  if (h < 18) return "Good afternoon";
  return "Good evening";
}

export function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n - 1).trimEnd() + "…" : s;
}

/** Parse streamed (possibly incomplete) JSON; returns undefined while invalid. */
export function tryParseJson(s: string): unknown {
  try {
    return JSON.parse(s);
  } catch {
    return undefined;
  }
}

export function startOfToday(now = Date.now()): number {
  const d = new Date(now);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/**
 * This device's time zone over `from`..`to` as the server takes it (`get_usage`'s `utc_offsets`):
 * the offset from UTC (minutes east) at the start, then each change (summer time) at the minute
 * it happens, so the server counts days as this device does. "All" (from 0) looks back five years.
 */
export function utcOffsets(from: number, to: number = Date.now()): { since: number; minutes: number }[] {
  const MIN = 60_000;
  const off = (t: number) => -new Date(t).getTimezoneOffset();
  const start = Math.max(from, to - 5 * 366 * 86_400_000);
  const out = [{ since: 0, minutes: off(start) }];
  // Every 12 hours (changes are months apart), then the exact minute of a change.
  for (let t = start; t < to; ) {
    const next = Math.min(t + 12 * 3_600_000, to);
    const was = out[out.length - 1].minutes;
    const now = off(next);
    if (now !== was) {
      let lo = Math.floor(t / MIN);
      let hi = Math.ceil(next / MIN);
      while (hi - lo > 1) {
        const mid = Math.floor((lo + hi) / 2);
        if (off(mid * MIN) === was) lo = mid;
        else hi = mid;
      }
      out.push({ since: hi * MIN, minutes: off(hi * MIN) });
    }
    t = next;
  }
  return out;
}

/** A dollar estimate: "$1.24", "$0.08", "<$0.01"; null when unknown or nothing. */
export function fmtCost(usd: number | undefined | null): string | null {
  if (typeof usd !== "number" || !Number.isFinite(usd) || usd <= 0) return null;
  if (usd < 0.01) return "<$0.01";
  if (usd >= 100) return `$${Math.round(usd)}`;
  return `$${usd.toFixed(2)}`;
}

/** "1 duty", "3 duties". */
export const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/**
 * Base64 data as a Blob. Not `fetch("data:…")`: the app's content policy
 * only lets it talk to its own channel, so that fetch is refused there.
 */
export function base64Blob(data: string, mime: string): Blob {
  const bin = atob(data);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new Blob([bytes], { type: mime });
}
