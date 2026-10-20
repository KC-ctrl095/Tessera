// Per-model reasoning modes. Each model advertises exactly the levels it
// supports (`ModelInfo.thinking`) with provider-native labels; the UI only
// ever offers those. Generic labels are a fallback for unknown models.
import type { ModelInfo } from "../proto/ModelInfo";
import type { ThinkingLevel } from "../proto/ThinkingLevel";
import type { ThinkingOption } from "../proto/ThinkingOption";

export const LEVELS: ThinkingLevel[] = ["auto", "off", "minimal", "low", "medium", "high", "xhigh", "max"];

const GENERIC: Record<ThinkingLevel, { label: string; hint: string }> = {
  auto: { label: "Server default", hint: "Use whatever the server is configured for" },
  off: { label: "Off", hint: "Answer directly, no reasoning; fastest" },
  minimal: { label: "Minimal", hint: "A very short think before answering" },
  low: { label: "Low", hint: "Quick, light reasoning" },
  medium: { label: "Medium", hint: "Balanced speed and depth" },
  high: { label: "High", hint: "Thorough reasoning for hard problems" },
  xhigh: { label: "Extra high", hint: "Very thorough; best for complex coding and agentic work" },
  max: { label: "Max", hint: "Maximum depth; slowest and most expensive" },
};

export const genericThinking = (l: ThinkingLevel) => GENERIC[l] ?? { label: l, hint: "" };

/** The modes a model supports (empty = the model has no reasoning control). */
export function thinkingOptions(model: ModelInfo | undefined): ThinkingOption[] {
  return model?.thinking ?? [];
}

const rank = (l: ThinkingLevel) => LEVELS.indexOf(l);

/**
 * The supported level closest to `want`, mirroring the server's snapping
 * (ties go to the cheaper level; never silently turns reasoning off).
 */
export function clampThinking(want: ThinkingLevel, options: ThinkingOption[], def: ThinkingLevel = "auto"): ThinkingLevel {
  if (!options.length) return "auto";
  if (options.some((o) => o.level === want)) return want;
  if (want === "auto") return options.some((o) => o.level === def) ? def : options[0].level;
  let best: ThinkingOption | null = null;
  let bestKey = Infinity;
  for (const o of options) {
    if (o.level === "auto" || o.level === "off") continue;
    const key = Math.abs(rank(o.level) - rank(want)) * 2 + (rank(o.level) > rank(want) ? 1 : 0);
    if (key < bestKey) {
      bestKey = key;
      best = o;
    }
  }
  return best?.level ?? options[0].level;
}

/** Label for a level as this model names it. */
export function thinkingLabel(level: ThinkingLevel, model: ModelInfo | undefined): string {
  return model?.thinking?.find((o) => o.level === level)?.label ?? genericThinking(level).label;
}

/** Short label without the "(model default)" suffix, for chips. */
export const shortLabel = (label: string) => label.replace(/\s*\(model default\)\s*$/i, "");

/** Parse a user-typed level ("xhigh", "extra high", "none", "default"…). */
export function parseThinking(s: string, model?: ModelInfo): ThinkingLevel | null {
  const v = s.trim().toLowerCase().replace(/[\s_-]+/g, "");
  if (!v) return null;
  const alias: Record<string, ThinkingLevel> = {
    auto: "auto",
    default: "auto",
    serverdefault: "auto",
    off: "off",
    none: "off",
    no: "off",
    minimal: "minimal",
    min: "minimal",
    low: "low",
    medium: "medium",
    med: "medium",
    high: "high",
    on: "high",
    xhigh: "xhigh",
    extrahigh: "xhigh",
    max: "max",
    maximum: "max",
  };
  if (alias[v]) return alias[v];
  const hit = model?.thinking?.find((o) => shortLabel(o.label).toLowerCase().replace(/[\s_-]+/g, "") === v);
  return hit?.level ?? null;
}
