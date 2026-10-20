// What a stored avatar value is, without the portraits themselves: the app's
// first download needs only these few string checks (labels, menus); the
// designs come with the portraits (catalog.ts), loaded once a bot is shown.

const MOSAIC_RE = /^m:[a-z0-9-]{1,30}$/;

/** Whether a stored avatar is a mosaic portrait reference (`m:<id>`), known or not. */
export const isMosaicAvatar = (v: string | null | undefined): boolean => !!v && MOSAIC_RE.test(v);

/** The id inside an `m:<id>` value, or null. */
export const mosaicId = (v: string | null | undefined): string | null => (v && MOSAIC_RE.test(v) ? v.slice(2) : null);

/** The avatar as plain text, for places that can't show an image (`<option>`, palette labels): "" for portraits. */
export const avatarGlyph = (avatar: string | null | undefined): string => (isMosaicAvatar(avatar) ? "" : (avatar ?? "").trim());
