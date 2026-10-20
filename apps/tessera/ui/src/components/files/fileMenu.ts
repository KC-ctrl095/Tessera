// A file's menu in a chat (an agent's shared file, a web view download, a
// file the user attached): every way it can open on this device, then
// Download and "Save as note". The way set in Settings comes first.
import { OPEN_LABEL, defaultOpen, isPictureFile, isTextLike, openFile, openPlaces, saveAsNote, saveFile, viewPicture, type OpenIn, type ViewFile } from "../../lib/fileOpen";
import { isTauri, isTauriMobile } from "../../lib/util";
import type { IconName } from "../icons";
import { openMenu, type Anchor, type MenuItem } from "../primitives";

const ICON: Record<OpenIn, IconName> = { tile: "mosaic", window: "appWindow", app: "external" };

/** The menu's items; `download` replaces the plain Download (a file card's own, with its progress). */
export function fileMenuItems(f: ViewFile, download?: () => void): MenuItem[] {
  const can = openPlaces(f);
  const first = defaultOpen(f);
  const items: MenuItem[] = [];
  if (isPictureFile(f) && (f.id || f.text !== undefined)) items.push({ label: "View", icon: "maximize", onSelect: () => void viewPicture(f) });
  const order: OpenIn[] = ["tile", "window", "app"];
  if (first) order.sort((a, b) => (a === first ? -1 : b === first ? 1 : 0));
  for (const how of order) {
    if (!can[how]) continue;
    items.push({ label: OPEN_LABEL[how], icon: ICON[how], description: how === first ? "What a tap on the file does (Settings → Appearance)" : undefined, onSelect: () => void openFile(f, how) });
  }
  if (items.length) items.push({ separator: true });
  if (download) items.push({ label: isTauriMobile ? "Save to phone" : isTauri ? "Save on this computer" : "Download", icon: "download", onSelect: download });
  else if (f.id || f.text !== undefined) items.push({ label: isTauriMobile ? "Save to phone" : "Download", icon: "download", onSelect: () => void saveFile(f) });
  if (isTextLike(f) && (f.id || f.text !== undefined)) items.push({ label: "Save as note", icon: "note", description: "A copy to write in", onSelect: () => void saveAsNote(f) });
  return items;
}

/** Open a file's menu at `anchor`: a small menu beside it, on phones too (no sheet). */
export function openFileMenu(anchor: Anchor, f: ViewFile, download?: () => void) {
  openMenu(anchor, fileMenuItems(f, download), "bottom-end", { sheet: false });
}
