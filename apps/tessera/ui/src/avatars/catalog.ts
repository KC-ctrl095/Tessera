// The avatar library and the `m:<id>` value format shared with the server.
import { hash, type Design } from "./engine";
import { FIGURES } from "./designs/figures";
import { CREATURES } from "./designs/creatures";
import { EMBLEMS } from "./designs/emblems";
import { mosaicId } from "./value";

export { avatarGlyph, isMosaicAvatar, mosaicId } from "./value";

export type AvatarCategory = "figure" | "creature" | "emblem";

export interface AvatarInfo {
  /** Neutral description of the look, e.g. "laurel-beard". Store as `m:<id>`. */
  id: string;
  category: AvatarCategory;
}

const byId = new Map<string, Design>();
const infos: AvatarInfo[] = [];
for (const [list, category] of [
  [FIGURES, "figure"],
  [CREATURES, "creature"],
  [EMBLEMS, "emblem"],
] as const) {
  for (const d of list) {
    byId.set(d.id, d);
    infos.push({ id: d.id, category });
  }
}

/** Every mosaic avatar, figures first, then creatures, then emblems. */
export const ALL_AVATARS: readonly AvatarInfo[] = infos;

/** Section labels for the picker. */
export const CATEGORY_LABEL: Record<AvatarCategory, string> = {
  figure: "Figures",
  creature: "Creatures",
  emblem: "Emblems",
};

/** The design for an id, if this build knows it. */
export const designFor = (id: string): Design | undefined => byId.get(id);

/** Whether this build can draw the given avatar value. */
export const isKnownAvatar = (v: string | null | undefined) => !!designFor(mosaicId(v) ?? "");

/**
 * A mosaic avatar value (`m:<id>`). With a seed (a bot id, a name) the choice is stable;
 * without one it is random. Optionally limited to one category.
 */
export function randomAvatar(seed?: string, category?: AvatarCategory): string {
  const pool = category ? infos.filter((a) => a.category === category) : infos;
  const i = seed === undefined ? Math.floor(Math.random() * pool.length) : hash(seed) % pool.length;
  return "m:" + pool[i].id;
}

const TEXT_RE = /^[\p{L}\p{N}]$/u;

/**
 * What to draw for a stored avatar. Empty values and single letters/digits (the server's fallback
 * from the bot's name) become a stable portrait chosen by `seed` (use the bot id); anything else is
 * shown as stored.
 */
export function resolveAvatar(avatar: string | null | undefined, seed: string): string {
  const v = (avatar ?? "").trim();
  return !v || TEXT_RE.test(v) ? randomAvatar(seed) : v;
}
