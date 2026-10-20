// Search hits and previews are one line of plain text: markdown marks are dropped and the matching
// words are picked out. The server marks a hit with «…» (FTS5); without marks the query's words are used.

/** Inline markdown as plain words. Literal angle brackets stay: Solid renders every piece as text. */
export function plainMarkdown(s: string): string {
  return s
    // Drop fence lines (including their language), not the code or a hit inside it.
    .replace(/^[ \t]{0,3}(?:`{3,}|~{3,})[^\n]*$/gm, " ")
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/g, (_, a: string, b?: string) => b ?? a)
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/^\s{0,3}(?:#{1,6}|>|[-*+]|\d+[.)])\s+/gm, "")
    .replace(/(\*\*|__|~~)/g, "")
    // Emphasis marks at the edge of a word; snake_case_names keep theirs.
    .replace(/(^|[\s(])[*_]+(?=\S)/g, "$1")
    .replace(/(\S)[*_]+(?=$|[\s).,;:!?])/g, "$1")
    .replace(/`+/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

export interface Piece {
  text: string;
  hit: boolean;
}

/** What a hit or preview shows: its plain words in pieces, the matches marked. */
export const pieces = (text: string, query?: string, raw?: boolean): Piece[] => highlight(raw ? text : plainMarkdown(text), query);

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** `text` cut into pieces, the matches marked: «…» from the server, else the words of `query`. */
export function highlight(text: string, query = ""): Piece[] {
  if (text.includes("«")) {
    const out: Piece[] = [];
    for (const [i, part] of text.split(/[«»]/).entries()) if (part) out.push({ text: part, hit: i % 2 === 1 });
    return out;
  }
  const words = query.split(/\s+/).filter(Boolean).map(escapeRe);
  if (!words.length) return [{ text, hit: false }];
  const re = new RegExp(`(${words.join("|")})`, "gi");
  // With a capture group the odd pieces are the matches (an empty piece still holds its place).
  return text
    .split(re)
    .map((t, i) => ({ text: t, hit: i % 2 === 1 }))
    .filter((p) => p.text);
}
