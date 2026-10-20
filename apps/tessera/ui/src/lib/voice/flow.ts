// Pure parts of the conversation, also exercised without a microphone.
export type Phase = "idle" | "starting" | "listening" | "sending" | "waiting" | "speaking";
export interface View { phase: Phase; conversation: boolean; level: number; note: string }
export const idle = (): View => ({ phase: "idle", conversation: false, level: 0, note: "" });

export function compose(prefix: string, text: string, suffix: string): string {
  return `${prefix}${prefix && text && !/\s$/.test(prefix) ? " " : ""}${text}${text && suffix && !/^\s/.test(suffix) ? " " : ""}${suffix}`;
}

export class Pauses {
  private began = 0;
  private last = 0;
  private first = 0;
  reset() { this.began = this.last = this.first = 0; }
  tick(level: number, now: number): { interrupt: boolean; finish: boolean } {
    if (level > 0.12) {
      if (!this.began) this.began = now;
      if (!this.first) this.first = now;
      this.last = now;
    } else this.began = 0;
    return { interrupt: !!this.began && now - this.began >= 450, finish: !!this.last && (now - this.last >= 1000 || now - this.first >= 115000) };
  }
  get heard() { return !!this.last; }
}

/** Only complete sentences during a stream, bounded chunks for long prose. */
export function sentence(text: string, final: boolean): { text: string; used: number } | null {
  const match = /[.!?](?:["')\]]*)(?:\s|$)|\n\n/.exec(text);
  let used = match ? match.index + match[0].length : final ? text.length : 0;
  if ((!used || used > 600) && text.length > 600) used = text.lastIndexOf(" ", 600) > 100 ? text.lastIndexOf(" ", 600) + 1 : 600;
  return used ? { text: text.slice(0, used), used } : null;
}

export function spoken(text: string): string {
  return text.replace(/```[\s\S]*?(?:```|$)/g, " ").replace(/!\[[^\]]*\]\([^)]*\)/g, " ").replace(/\[([^\]]+)\]\([^)]*\)/g, "$1").replace(/https?:\/\/\S+/g, "link").replace(/[*_`#>|]/g, "").replace(/\s+/g, " ").trim();
}

/** Ignore the recognizer repeating words currently coming out of the speaker. */
export function echo(words: string, answer: string): boolean {
  const clean = (s: string) => s.toLowerCase().replace(/[^\p{L}\p{N} ]/gu, " ").replace(/\s+/g, " ").trim();
  const w = clean(words), a = clean(answer);
  return w.length >= 8 && a.includes(w);
}
