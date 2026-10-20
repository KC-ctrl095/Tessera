// The remote browser (shared contract, batch 3 §1): the server runs one
// Chromium-family browser with a live tab per open web view board; a web
// view tile shows that tab's screencast and sends input back. Everything
// goes over the normal connection, so it works the same in the apps and in a
// plain browser.
//
// · attach/detach: a tile on screen attaches with its size, device pixel
//   ratio and device type; only attached connections get frames.
// · frames are acknowledged once drawn or skipped for a newer one
//   (`browser_ack`). We take several at a time (`pipelined`): the server
//   keeps as many on their way as the connection carries without queueing,
//   so a slow device never builds a backlog and a far one isn't held to one
//   frame per round trip. Once a page stops moving, a sharp WebP of it
//   follows (`formats`).
// · input is fire-and-forget; pointer and finger moves and wheel turns are
//   folded into one per animation frame.
//
// Frame and input wire shapes stay here; this loads with the web-view tile.
import type { BrowserInputEvent } from "../proto/BrowserInputEvent";
import type { BrowserMouseButton } from "../proto/BrowserMouseButton";
import type { BrowserNavAction } from "../proto/BrowserNavAction";
import type { BrowserTab } from "../proto/BrowserTab";
import type { BrowserPageAction } from "../proto/BrowserPageAction";
import type { BrowserPageResult } from "../proto/BrowserPageResult";
import type { BrowserUploadAction } from "../proto/BrowserUploadAction";
import type { BrowserUploadResult } from "../proto/BrowserUploadResult";
import type { Event } from "../proto/Event";
import type { Whiteboard } from "../proto/Whiteboard";
import { backend } from "./backend";
import { keepTab as keep } from "./browser";
import { call, setState } from "./store";

export type { BrowserTab, BrowserInputEvent, BrowserNavAction };
export type BrowserFrame = Extract<Event, { type: "browser_frame" }>;
export type BrowserFocus = Extract<Event, { type: "browser_focus" }>;
export type BrowserDialog = Extract<Event, { type: "browser_dialog" }>;
/** Modifier bits: Alt 1, Ctrl 2, Meta 4, Shift 8. */
export type Modifiers = number;
export type MouseButton = BrowserMouseButton;

export { tabs, forgetTab, toAddress, displayAddress } from "./browser";

// ── views (what this device has attached) ────────────────────────────────

export interface ViewHandlers {
  /** Resolves once drawn (or skipped for a newer one). */
  frame(f: BrowserFrame): Promise<void>;
  focus?(f: BrowserFocus): void;
  /** The page asks (or null: it was answered, here or on another device). */
  dialog?(d: BrowserDialog | null): void;
}
interface View {
  boardId: string;
  size: { width: number; height: number; dpr: number; mobile: boolean };
  handlers: ViewHandlers;
}
/** One attachment per board on this device (two tiles showing one board share it). */
const views = new Map<string, Set<View>>();

/** Start receiving a board's frames at this size; returns the tab. Call again when the size changes. */
export async function attach(boardId: string, size: View["size"], handlers: ViewHandlers, signal?: AbortSignal): Promise<{ view: View; tab: BrowserTab | null }> {
  const view: View = { boardId, size, handlers };
  let set = views.get(boardId);
  if (!set) views.set(boardId, (set = new Set()));
  set.add(view);
  const cancel = () => detach(view);
  signal?.addEventListener("abort", cancel, { once: true });
  try {
    if (signal?.aborted) throw new DOMException("View closed", "AbortError");
    const tab = await sendAttach(view);
    if (signal?.aborted) {
      // The remote attach may have finished after the earlier detach.
      if (!views.has(boardId)) void call({ method: "browser_detach", params: { board_id: boardId } }).catch(() => {});
      throw new DOMException("View closed", "AbortError");
    }
    return { view, tab };
  } catch (e) {
    detach(view);
    throw e;
  } finally {
    signal?.removeEventListener("abort", cancel);
  }
}
async function sendAttach(v: View): Promise<BrowserTab | null> {
  const w = Math.max(1, Math.round(v.size.width));
  const h = Math.max(1, Math.round(v.size.height));
  const formats = (await webp) ? ["image/webp"] : [];
  if (!views.get(v.boardId)?.has(v)) throw new DOMException("View closed", "AbortError");
  return call<BrowserTab>({
    method: "browser_attach",
    params: { board_id: v.boardId, width: w, height: h, dpr: v.size.dpr, mobile: v.size.mobile, pipelined: true, formats },
  }).then(keep);
}

/** This device draws WebP (still pictures come as WebP then). */
const webp: Promise<boolean> =
  typeof createImageBitmap === "undefined"
    ? Promise.resolve(false)
    : createImageBitmap(frameBlob("UklGRiIAAABXRUJQVlA4IBYAAAAwAQCdASoBAAEADsD+JaQAA3AAAAAA", "image/webp")).then(
        (b) => (b.close(), true),
        () => false,
      );

/** A frame's picture as a Blob (decoded off the main thread by createImageBitmap). */
export function frameBlob(data: string, mime: string): Blob {
  const native = (Uint8Array as unknown as { fromBase64?: (s: string) => Uint8Array }).fromBase64;
  if (native) return new Blob([native(data) as BlobPart], { type: mime });
  const bin = atob(data);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new Blob([bytes], { type: mime });
}
/** The tile changed size (or device type): the tab follows. */
export function resize(v: View, size: View["size"]) {
  v.size = size;
  return sendAttach(v);
}
/** Show a web view whose machine is offline in the server's browser instead (it opens its address there). */
export const toServer = (boardId: string) => call<Whiteboard>({ method: "browser_to_server", params: { board_id: boardId } });
/** Show the page in a normal browser window on the computer its web view runs on (`show` false: close it, back to the tile). */
export const showOnScreen = (boardId: string, show: boolean) => call<null>({ method: "browser_show_on_screen", params: { board_id: boardId, show } });

/** Stop receiving frames (the tile went off screen or closed). */
export function detach(v: View) {
  const set = views.get(v.boardId);
  if (!set?.delete(v)) return;
  if (set.size) return;
  views.delete(v.boardId);
  void call({ method: "browser_detach", params: { board_id: v.boardId } }).catch(() => {});
}

// ── input ────────────────────────────────────────────────────────────────

type Foldable = Extract<BrowserInputEvent, { type: "mouse" | "touch" }>;
const pending = new Map<string, Foldable>();
let raf = 0;
const flush = () => {
  raf = 0;
  for (const [boardId, ev] of pending) send(boardId, ev);
  pending.clear();
};
function send(boardId: string, event: BrowserInputEvent) {
  void call({ method: "browser_input", params: { board_id: boardId, event } }).catch(() => {});
}
/**
 * Send an input event. Moves (pointer and finger) and wheel turns wait for
 * the next frame and are folded together (the latest position, the summed
 * scroll); anything else goes at once, after them, so the order is kept.
 */
export function input(boardId: string, ev: BrowserInputEvent) {
  if ((ev.type === "mouse" && (ev.kind === "move" || ev.kind === "wheel")) || (ev.type === "touch" && ev.kind === "move")) {
    const prev = pending.get(boardId);
    if (prev && prev.type === ev.type && prev.kind === ev.kind) {
      if (ev.type === "mouse" && prev.type === "mouse" && ev.kind === "wheel") pending.set(boardId, { ...ev, dx: prev.dx + ev.dx, dy: prev.dy + ev.dy });
      else pending.set(boardId, ev);
    } else {
      if (prev) send(boardId, prev);
      pending.set(boardId, ev);
    }
    if (!raf) raf = requestAnimationFrame(flush);
    return;
  }
  const prev = pending.get(boardId);
  if (prev) {
    pending.delete(boardId);
    send(boardId, prev);
  }
  send(boardId, ev);
}

export function nav(boardId: string, action: BrowserNavAction) {
  return call<BrowserTab>({ method: "browser_nav", params: { board_id: boardId, action } }).then(keep);
}
export async function newTab(boardId: string, url: string) {
  const board = await call<Whiteboard>({ method: "browser_new_tab", params: { board_id: boardId, url } });
  setState("boards", board.id, board);
  return board;
}
export const pageAction = (boardId: string, action: BrowserPageAction) =>
  call<BrowserPageResult>({ method: "browser_page", params: { board_id: boardId, action } });
export function ack(boardId: string, frame: number) {
  void call({ method: "browser_ack", params: { board_id: boardId, frame } }).catch(() => {});
}
export function answerDialog(boardId: string, accept: boolean, text: string | null) {
  return call({ method: "browser_dialog_answer", params: { board_id: boardId, accept, ...(text != null ? { text } : {}) } });
}

/** Only files from the device's native picker, never a server/machine path. */
export async function chooseFiles(boardId: string, chooser: string, selected: File[], progress?: (got: number, total: number) => void, signal?: AbortSignal) {
  const total = selected.reduce((n, f) => n + f.size, 0);
  if (selected.length > 8 || total > 200 * 1024 * 1024) throw new Error("Choose at most 8 files totaling 200 MB.");
  if (total > 4 * 1024 * 1024) {
    const rpc = (action: BrowserUploadAction) => call<BrowserUploadResult>({ method: "browser_upload", params: { board_id: boardId, chooser, action } });
    let upload: string | undefined;
    const alive = () => { if (signal?.aborted) throw new Error("Upload canceled."); };
    try {
      alive();
      upload = (await rpc({ type: "start", files: selected.map((f) => ({ name: f.name, size: f.size })) })).upload;
      if (!upload) throw new Error("This web-view browser couldn't start the upload.");
      progress?.(0, total);
      for (let index = 0; index < selected.length; index++) {
        const file = selected[index];
        for (let offset = 0; offset < file.size; offset += 256 * 1024) {
          alive();
          const bytes = new Uint8Array(await file.slice(offset, offset + 256 * 1024).arrayBuffer());
          let raw = "";
          for (let i = 0; i < bytes.length; i += 0x8000) raw += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
          const sent = await rpc({ type: "chunk", upload, index, data: btoa(raw) });
          progress?.(sent.received, total);
        }
      }
      alive();
      await rpc({ type: "finish", upload });
      return;
    } catch (e) {
      if (upload) await rpc({ type: "cancel", upload }).catch(() => {});
      throw e;
    }
  }
  if (signal?.aborted) throw new Error("Upload canceled.");
  const files = await Promise.all(selected.map(async (f) => {
    const bytes = new Uint8Array(await f.arrayBuffer());
    let raw = "";
    for (let i = 0; i < bytes.length; i += 0x8000) raw += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    return { name: f.name, mime: f.type || "application/octet-stream", data: btoa(raw) };
  }));
  return call({ method: "browser_choose_files", params: { board_id: boardId, chooser, files } });
}

// ── events ───────────────────────────────────────────────────────────────

backend.onEvent((e) => {
  switch (e.type) {
    case "browser_frame": {
      const set = views.get(e.board_id);
      // Not ours (any more): let the server move on.
      if (!set?.size) return ack(e.board_id, e.frame);
      // Every tile showing it draws; the next frame is asked for once the first has.
      const [lead, ...rest] = set;
      for (const v of rest) void v.handlers.frame(e);
      void lead.handlers.frame(e).finally(() => ack(e.board_id, e.frame));
      return;
    }
    case "browser_focus":
      for (const v of views.get(e.board_id) ?? []) v.handlers.focus?.(e);
      return;
    case "browser_dialog":
      for (const v of views.get(e.board_id) ?? []) v.handlers.dialog?.(e);
      return;
    case "browser_dialog_closed":
      for (const v of views.get(e.board_id) ?? []) v.handlers.dialog?.(null);
      return;
  }
});

// Back after a dropped connection: attach again (the server forgot us).
let wasConnected = false;
backend.onStatus((s) => {
  const now = s.state === "connected";
  if (now && !wasConnected) for (const set of views.values()) for (const v of set) void sendAttach(v).catch(() => {});
  wasConnected = now;
});

/** Modifier bits of a DOM event. */
export const modifiersOf = (e: { altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean }): Modifiers =>
  (e.altKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.metaKey ? 4 : 0) | (e.shiftKey ? 8 : 0);

export const buttonOf = (b: number): MouseButton => (b === 0 ? "left" : b === 1 ? "middle" : b === 2 ? "right" : b === 3 ? "back" : b === 4 ? "forward" : "none");
