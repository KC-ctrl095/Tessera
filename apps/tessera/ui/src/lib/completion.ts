// Completion acknowledgements are protocol, not conversation content.
import type { Entry } from "../proto/Entry";

/** Narrow fallback for servers/history predating the source marker. */
export function isCheckAck(text: string): boolean {
  const t = text.trim();
  return /[✓✔✅]/u.test(t) && !t.includes("\n") && /^(?:done|checked|ok|okay|acknowledged)?$/i.test(t.replace(/^[✓✔✅.!\s]+|[✓✔✅.!\s]+$/gu, ""));
}

/** Buffer only an acknowledgement-shaped prefix, never arbitrary short answers. */
export function possibleCheckAck(text: string): boolean {
  const t = text.trim().replace(/^[✓✔✅.!\s]+|[✓✔✅.!\s]+$/gu, "").toLowerCase();
  return !t.includes("\n") && ["done", "checked", "ok", "okay", "acknowledged"].some((word) => word.startsWith(t));
}

export function isProtocolEntry(e: Entry): boolean {
  if (e.kind === "user") return e.author === "harness" && e.content.some((p) => p.type === "text" && p.text.trimStart().startsWith("[Completion check]"));
  return e.kind === "assistant" && (e.author === "harness:completion" ||
    (!e.content.some((p) => p.type === "tool_call") && isCheckAck(e.content.filter((p) => p.type === "text").map((p) => p.text).join(""))));
}
