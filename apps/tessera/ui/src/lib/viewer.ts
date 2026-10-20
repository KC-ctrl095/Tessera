// The picture viewer: one full-screen viewer for every image in a chat (the
// user's attachments, pictures in the agent's replies, screenshots on tool
// cards). It lives outside the tiles, so nothing in a tile (a scroller, the
// phone's pager, a message that skips painting while off screen) can clip it,
// move it or take its touches. See components/Lightbox.tsx.
import { createSignal } from "solid-js";

export interface ViewImage {
  src: string;
  alt: string;
}

const [viewer, setViewer] = createSignal<{ images: ViewImage[]; index: number } | null>(null);
export { viewer };

/** Show `images`, starting at `index`. */
export function openViewer(images: ViewImage[], index = 0) {
  if (!images.length) return;
  setViewer({ images, index: Math.max(0, Math.min(index, images.length - 1)) });
}

export function closeViewer() {
  setViewer(null);
}

/** Go to another image of the open set. */
export function showImage(index: number) {
  const v = viewer();
  if (v && index >= 0 && index < v.images.length && index !== v.index) setViewer({ ...v, index });
}

/**
 * Images that open in the viewer when clicked: a message's attached images,
 * pictures in the agent's replies, a tool's output images.
 */
export const VIEWABLE = ".bubble-img img, .tool-images img, .msg-assistant img";

const viewImageOf = (img: HTMLImageElement): ViewImage => ({ src: img.currentSrc || img.src, alt: img.alt || "Image" });

/** The images of the chat around `img`, in reading order, and where `img` is among them. */
export function chatImages(img: HTMLImageElement): { images: ViewImage[]; index: number } {
  const root = img.closest(".transcript") ?? img.parentElement;
  const all = root ? Array.from(root.querySelectorAll<HTMLImageElement>(VIEWABLE)).filter((i) => i.src && !i.closest("[data-noview]")) : [img];
  const index = all.indexOf(img);
  return index < 0 ? { images: [viewImageOf(img)], index: 0 } : { images: all.map(viewImageOf), index };
}

/** A click (or tap) on a viewable image anywhere opens the viewer with that chat's images. */
export function installViewerClicks(): () => void {
  const onClick = (e: MouseEvent) => {
    if (e.defaultPrevented || e.button !== 0) return;
    const t = e.target as Element | null;
    const hit = t?.closest?.(".bubble-img, .tool-images img, .msg-assistant img");
    if (!hit || hit.closest("[data-noview]")) return;
    const img = hit instanceof HTMLImageElement ? hit : hit.querySelector("img");
    if (!img?.src) return;
    // A long press opened the message's menu: that tap was for the menu.
    if (document.querySelector(".menu-pop")) return;
    e.preventDefault();
    const { images, index } = chatImages(img);
    openViewer(images, index);
  };
  document.addEventListener("click", onClick);
  return () => document.removeEventListener("click", onClick);
}

/** A file name for saving an image: its kind from the data URL ("image-2026-10-02-0412.jpg"). */
export function imageFileName(src: string): string {
  const mime = /^data:([^;,]+)/.exec(src)?.[1] ?? "image/png";
  const ext = { "image/jpeg": "jpg", "image/png": "png", "image/gif": "gif", "image/webp": "webp", "image/svg+xml": "svg" }[mime] ?? "png";
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `image-${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}.${ext}`;
}

/** The pure fitting maths (unit-tested in e2e): an image of `nw`×`nh` in a `bw`×`bh` box, never enlarged past `max`. */
export function fitSize(nw: number, nh: number, bw: number, bh: number, max = 1): { w: number; h: number; k: number } {
  if (!(nw > 0 && nh > 0 && bw > 0 && bh > 0)) return { w: 0, h: 0, k: 1 };
  const k = Math.min(bw / nw, bh / nh, max);
  return { w: nw * k, h: nh * k, k };
}
