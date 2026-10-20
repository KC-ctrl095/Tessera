// A note tile: a markdown document from the notes library, open to write in
// (NoteEditor: a live page, Obsidian-style; read-only from the ⋯ menu). It
// saves on its own a moment after you stop typing.
//
// Changes made elsewhere (another device, an agent's `note` tool) follow it
// in (lib/noteSync.ts): quietly when nothing is typed here or they touch
// other lines, with a question only when both changed the same lines. Its own
// saves coming back are never mistaken for someone else's. An agent's changes
// can be looked at and undone; earlier versions are kept (Version history).
import { For, Match, Show, Suspense, Switch, createEffect, createMemo, createSignal, lazy, on, onCleanup, onMount } from "solid-js";
import type { Note } from "../../proto/Note";
import type { NoteInfo } from "../../proto/NoteInfo";
import type { NoteVersion } from "../../proto/NoteVersion";
import { agentName, backlinks, infoOf, isReadOnly, markReviewed, NOTE_MAX, noteFileName, reviewedUpTo, setReadOnly } from "../../lib/notes";
import { assetBytes, assetRefs } from "../../lib/noteAssets";
import { noteDraft, keepNoteDraft, noteDraftSaved, discardNoteDraft } from "../../lib/noteDrafts";
import { connectionGeneration } from "../../lib/connectionGeneration";
import { noteKey, reconcile, serverTitle } from "../../lib/noteSync";
import { merge3 } from "../../lib/textDiff";
import { api, botById, onBootstrapped, onNoteEvent, setState, state } from "../../lib/store";
import { confirmDialog, setDrawerOpen, toast, toastError } from "../../lib/ui";
import { copyText, debounce, relTime, touchUi } from "../../lib/util";
import { saveBlob, saveText } from "../../lib/files";
import { zip } from "../../lib/zip";
import { closePane, focusComposer, getDraft, openSession, paneShowing, setDraft, workspace, workspaceServer } from "../../lib/workspace";
import { Icon, Tessellate } from "../icons";
import { usePane } from "../PaneContext";
import { RestoreButton } from "../RestoreButton";
import { openMenu, type MenuItem } from "../primitives";
import { ALT } from "../TopBar";
import { NoteDiff } from "./NoteDiff";
import type { NoteEditorHandle } from "./NoteEditor";
import "../../styles/notes.css";
import "../../styles/find.css";
import { NewTileButton, newInHeader } from "../NewMenu";

/** The editor and everything it needs, loaded with the first note opened. */
const NoteEditor = lazy(() => import("./NoteEditor"));

type SaveState = "saved" | "dirty" | "saving" | "conflict" | "error";

/** An agent's changes, to look at or undo: the text before and after them. */
type Review = { before: string; beforeTitle: string; after: string; afterTitle: string; who: string; version: number };

type View = { kind: "doc" } | { kind: "review" } | { kind: "compare" } | { kind: "history" } | { kind: "version"; v: NoteVersion } | { kind: "outline" };

export function NoteTile(props: { paneId: string }) {
  const pane = usePane();
  const noteId = () => {
    const c = workspace.panes[props.paneId]?.content;
    return c?.type === "note" ? c.noteId : "";
  };
  /** The version the text here was last in step with (its base). */
  const [note, setNote] = createSignal<Note | null>(null);
  const [missing, setMissing] = createSignal(false);
  const [loadFailed, setLoadFailed] = createSignal(false);
  const [recovered, setRecovered] = createSignal(false);
  let draftServer = workspaceServer() ?? "";
  let disposed = false;
  let openGeneration = 0;
  const [title, setTitle] = createSignal("");
  const [text, setText] = createSignal("");
  const [save, setSave] = createSignal<SaveState>("saved");
  /** A newer version that changed the same lines as what's typed here. */
  const [theirs, setTheirs] = createSignal<Note | null>(null);
  const [review, setReview] = createSignal<Review | null>(null);
  const [view, setView] = createSignal<View>({ kind: "doc" });
  /** Said for a moment where the save status is ("Combined with…"). */
  const [notice, setNotice] = createSignal("");
  const readOnly = () => isReadOnly(noteId());
  let titleInput: HTMLInputElement | undefined;
  const [doc, setDoc] = createSignal<NoteEditorHandle>();
  /** The notes that link here ("Linked from"). */
  const [links, setLinks] = createSignal<NoteInfo[] | null>(null);
  const loadLinks = async () => {
    const id = noteId();
    if (!id) return;
    const list = await backlinks(id);
    if (id === noteId()) setLinks(list);
  };
  const loadLinksSoon = debounce(() => void loadLinks(), 1500);
  onCleanup(() => loadLinksSoon.cancel());

  // ── keeping in step ────────────────────────────────────────────────────
  /** A save is on its way. */
  let saving = false;
  /** A newer version was announced while saving: fetch it after. */
  let behind = false;
  let fetching = false;
  let refetch = false;
  /** Fingerprints of what this tile sent lately (its own saves, answered or not). */
  const sent: number[] = [];
  const draft = () => ({ title: serverTitle(title()), content: text() });
  const rememberDraft = () => {
    const base = note();
    if (base) keepNoteDraft(draftServer, { base, ...draft() });
  };
  const unsaved = () => {
    const n = note();
    return !!n && (serverTitle(title()) !== n.title || text() !== n.content);
  };
  let noticeTimer: ReturnType<typeof setTimeout> | undefined;
  const say = (msg: string) => {
    setNotice(msg);
    clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => setNotice(""), 4000);
  };
  onCleanup(() => clearTimeout(noticeTimer));

  /** An agent's change came in: keep it to look at (several in a row add up). */
  const offerReview = (before: Note, after: Note) => {
    if (!after.agent_edit || (before.content === after.content && before.title === after.title)) return;
    const r = review();
    setReview({
      before: r ? r.before : before.content,
      beforeTitle: r ? r.beforeTitle : before.title,
      after: after.content,
      afterTitle: after.title,
      who: agentName(after.edit_session),
      version: after.version,
    });
  };

  const open = async (id: string) => {
    rememberDraft();
    const server = workspaceServer() ?? "";
    const connection = connectionGeneration();
    const generation = ++openGeneration;
    const kept = noteDraft(server, id);
    if (draftServer !== server || note()?.id !== id) {
      setNote(null);
      setTitle("");
      setText("");
      setTheirs(null);
      setMissing(false);
      setRecovered(false);
    }
    draftServer = server;
    if (kept) {
      draftServer = server;
      setNote(kept.base);
      setTitle(kept.title);
      setText(kept.content);
      setTheirs(null);
      setMissing(false);
      setSave("dirty");
      setRecovered(true);
    }
    const valid = () => !disposed && generation === openGeneration && connection === connectionGeneration() && server === (workspaceServer() ?? "") && id === noteId();
    setLoadFailed(false);
    try {
      const n = await api.getNote(id);
      if (!valid()) return;
      draftServer = server;
      const kept = noteDraft(server, id);
      const local = note()?.id === id && unsaved();
      setMissing(false);
      setState("notes", n.id, infoOf(n));
      setTheirs(null);
      setReview(null);
      setView({ kind: "doc" });
      sent.length = 0;
      if (local) {
        take(n);
        if (unsaved() && !theirs()) saveSoon();
      } else {
        setNote(kept?.base ?? n);
        setTitle(kept?.title ?? n.title);
        setText(kept?.content ?? n.content);
        setSave(kept ? "dirty" : "saved");
        setRecovered(!!kept);
        if (kept) {
          take(n);
          if (unsaved() && !theirs()) saveSoon();
        }
      }
      setLinks(null);
      void loadLinks();
      void lookForAgentChanges(n);
    } catch (e) {
      if (!valid()) return;
      if ((e as { code?: string }).code === "not_found") setMissing(true);
      else {
        setLoadFailed(true);
        if (kept) setSave("error");
        toastError(e, "Couldn't open the note");
      }
    }
  };
  createEffect(on(() => [workspaceServer(), noteId()] as const, ([, id]) => id && void open(id)));

  /** Opened after an agent changed it (and nobody looked yet here): offer its changes. */
  const lookForAgentChanges = async (n: Note) => {
    if (!n.agent_edit || n.version <= 1 || reviewedUpTo(n.id) >= n.version) return;
    try {
      const kept = await api.noteHistory(n.id);
      const from = kept.find((v) => !v.agent_edit) ?? kept[0];
      if (!from || note()?.version !== n.version) return;
      const v = await api.getNoteVersion(n.id, from.version);
      if (note()?.version !== n.version || review()) return;
      setReview({ before: v.content, beforeTitle: v.title, after: n.content, afterTitle: n.title, who: agentName(n.edit_session), version: n.version });
    } catch {
      /* an older server keeps no history */
    }
  };

  /** Fetch the latest version and take it in. */
  const pull = async () => {
    const id = noteId();
    if (!id || disposed) return;
    const connection = connectionGeneration();
    const server = draftServer;
    const valid = () => !disposed && id === noteId() && connection === connectionGeneration() && server === (workspaceServer() ?? "");
    if (!note()) return open(id);
    if (fetching) {
      refetch = true;
      return;
    }
    fetching = true;
    try {
      const n = await api.getNote(id);
      if (valid()) take(n);
    } catch (e) {
      if (valid() && (e as { code?: string }).code === "not_found") setMissing(true);
    } finally {
      fetching = false;
      if (refetch) {
        refetch = false;
        void pull();
      }
    }
  };

  /** A version from the server, maybe newer than the base. */
  const take = (n: Note) => {
    const base = note();
    if (!base) return;
    if (n.version >= (state.notes[n.id]?.version ?? 0)) setState("notes", n.id, infoOf(n));
    const waiting = theirs();
    if (waiting) {
      // Already asking: the question is about the newest version, unless it now says what's typed here.
      if (n.version <= waiting.version) return;
      const d = draft();
      if (d.title === n.title && d.content === n.content) {
        setTheirs(null);
        setNote(n);
        setSave("saved");
      } else setTheirs(n);
      return;
    }
    const d = reconcile(base, draft(), n, sent);
    switch (d.kind) {
      case "ignore":
        return;
      case "rebase":
        setNote(n);
        setSave(unsaved() ? "dirty" : "saved");
        if (unsaved()) saveSoon();
        return;
      case "follow":
        setNote(n);
        if (document.activeElement !== titleInput || serverTitle(title()) !== n.title) setTitle(n.title);
        setText(n.content);
        setSave("saved");
        offerReview(base, n);
        return;
      case "merge":
        setNote(n);
        if (d.title !== serverTitle(title())) setTitle(d.title);
        setText(d.content);
        setSave("dirty");
        saveSoon();
        offerReview(base, n);
        say(n.agent_edit ? `Combined with ${agentName(n.edit_session)}'s changes` : "Combined with changes from elsewhere");
        return;
      case "conflict":
        setTheirs(n);
        setSave("conflict");
        return;
    }
  };

  onMount(() => {
    const off = onNoteEvent((e) => {
      if (draftServer !== (workspaceServer() ?? "")) return;
      if (e.type === "note_removed" && e.id === noteId()) setMissing(true);
      // Another note changed: it may link here now (or no longer).
      if ((e.type === "note_updated" && e.note.id !== noteId()) || e.type === "note_removed") loadLinksSoon();
      if (e.type !== "note_updated" || e.note.id !== noteId()) return;
      // Who may read it changed (no new version).
      const cur = note();
      if (cur && ((e.note.all_bots !== undefined && e.note.all_bots !== cur.all_bots) || (e.note.keep_from_agents !== undefined && e.note.keep_from_agents !== cur.keep_from_agents)))
        setNote({ ...cur, all_bots: e.note.all_bots, keep_from_agents: e.note.keep_from_agents });
      if (e.note.version <= (note()?.version ?? 0)) return;
      // Our own save comes back before its answer: wait for the answer, then look.
      if (saving) behind = true;
      else void pull();
    });
    // Back after a dropped connection: catch up, then save what's waiting.
    const offBoot = onBootstrapped(() => {
      void pull().then(() => {
        if (unsaved() && !theirs()) saveSoon();
      });
    });
    onCleanup(() => {
      off();
      offBoot();
    });
  });

  const flush = async () => {
    const n = note();
    // While a newer version waits for "Keep mine" / "Use theirs", what's typed stays here.
    if (!n || theirs() || saving) return;
    if (!unsaved()) {
      if (save() !== "conflict") setSave("saved");
      return;
    }
    if (text().length > NOTE_MAX) {
      setSave("error");
      toast({ level: "warning", message: "This note is too long to save (about 200 KB at most)." });
      return;
    }
    const out = draft();
    const server = draftServer;
    const connection = connectionGeneration();
    const valid = () => !disposed && connection === connectionGeneration() && server === (workspaceServer() ?? "") && noteId() === n.id;
    rememberDraft();
    sent.push(noteKey(out.title, out.content));
    if (sent.length > 8) sent.shift();
    saving = true;
    setSave("saving");
    let retry = false;
    try {
      const saved = await api.saveNote({ id: n.id, title: out.title, content: out.content, base_version: n.version });
      noteDraftSaved(server, saved, out);
      if (!valid()) return;
      if (saved.version >= (note()?.version ?? 0)) setNote(saved);
      setState("notes", saved.id, infoOf(saved));
      setSave(unsaved() ? "dirty" : "saved");
    } catch (e) {
      if (!valid()) return;
      if ((e as { code?: string }).code === "conflict") {
        // Newer than our base: maybe our own earlier save whose answer got lost, maybe someone else's.
        saving = false;
        await pull();
        retry = true;
      } else {
        setSave("error");
        if ((e as { code?: string }).code !== "transport") toastError(e, "The note wasn't saved");
      }
    } finally {
      saving = false;
      if (!valid()) return;
      rememberDraft();
      if (behind) {
        behind = false;
        void pull();
      }
      if (unsaved() && !theirs() && (save() !== "error" || retry)) saveSoon();
    }
  };
  const saveSoon = debounce(() => void flush(), 700);
  const edited = () => {
    rememberDraft();
    if (save() !== "conflict") setSave("dirty");
    saveSoon();
  };
  // Leaving (tile closed, app backgrounded): save what's pending now.
  onCleanup(() => {
    rememberDraft();
    saveSoon.flush();
    saveSoon.cancel();
    disposed = true;
  });
  const onHidden = () => {
    if (document.visibilityState === "hidden") saveSoon.flush();
  };
  document.addEventListener("visibilitychange", onHidden);
  onCleanup(() => document.removeEventListener("visibilitychange", onHidden));

  // ── a conflict ─────────────────────────────────────────────────────────
  const keepMine = () => {
    const t = theirs();
    if (!t) return;
    setNote(t); // their version is the base now; ours goes on top of it
    setTheirs(null);
    setView({ kind: "doc" });
    setSave("dirty");
    void flush();
  };
  const useTheirs = () => {
    const t = theirs();
    if (!t) return;
    discardNoteDraft(draftServer, t.id);
    setRecovered(false);
    const base = note();
    setTheirs(null);
    setNote(t);
    setTitle(t.title);
    setText(t.content);
    setSave("saved");
    setView({ kind: "doc" });
    if (base) offerReview(base, t);
  };
  const whoElse = (n: Note) => (n.agent_edit ? agentName(n.edit_session) : "Someone");

  // ── an agent's changes ─────────────────────────────────────────────────
  const doneReview = () => {
    const r = review();
    if (r) markReviewed(noteId(), r.version);
    setReview(null);
    if (view().kind === "review") setView({ kind: "doc" });
  };
  /** Take the agent's changes back out, keeping what was typed since. */
  const undoAgent = () => {
    const r = review();
    if (!r) return;
    const m = merge3(r.after, text(), r.before);
    if (!m.ok) {
      toast({ level: "warning", title: "Couldn't undo just those changes", message: "You've changed the same lines since. Version history has the earlier text." });
      return;
    }
    doc()?.replace(m.text);
    setText(m.text);
    if (serverTitle(title()) === r.afterTitle && r.afterTitle !== r.beforeTitle) setTitle(r.beforeTitle);
    doneReview();
    edited();
    say(`Undid ${r.who}'s changes`);
  };

  // While its changes are offered, the agent's words are tinted in the note.
  createEffect(on([review, doc], ([r, d]) => d?.tint(r ? r.before : null)));

  // ── history ────────────────────────────────────────────────────────────
  const [history, setHistory] = createSignal<NoteVersion[] | null>(null);
  const openHistory = async () => {
    setView({ kind: "history" });
    setHistory(null);
    try {
      setHistory(await api.noteHistory(noteId()));
    } catch (e) {
      setHistory([]);
      toastError(e, "Couldn't load the note's history");
    }
  };
  const showVersion = async (v: NoteVersion) => {
    try {
      setView({ kind: "version", v: await api.getNoteVersion(noteId(), v.version) });
    } catch (e) {
      toastError(e, "Couldn't open that version");
    }
  };
  const restore = (v: NoteVersion) => {
    doc()?.replace(v.content);
    setText(v.content);
    setTitle(v.title);
    setView({ kind: "doc" });
    edited();
    say(`Restored the version from ${relTime(v.updated_at)}`);
  };
  const versionBy = (v: { agent_edit: boolean; edit_session?: string }) => (v.agent_edit ? agentName(v.edit_session) : "You");

  // ── the rest ───────────────────────────────────────────────────────────
  const by = () => {
    const n = note();
    if (!n) return "";
    if (n.agent_edit && n.version > 1) return `Edited by ${agentName(n.edit_session)}`;
    if (n.by_user) return "Written by you";
    const info = n.session_id ? state.sessions[n.session_id]?.info : undefined;
    const bot = botById(info?.bot_id);
    return `Written by ${bot?.name ?? "the agent"}`;
  };
  const status = () => notice() || ({ saved: "Saved", dirty: "Editing…", saving: "Saving…", conflict: "Changed elsewhere", error: "Not saved" })[save()];

  /**
   * "About the note …" in the message box of the chat it was made in: the agent reads it by its id (sent, it is
   * readable in that chat). Other chats get a note when the user links it in a message of theirs ([[Title]]).
   */
  const askAbout = () => {
    const n = note();
    if (!n?.session_id) return;
    const chat = paneShowing(n.session_id) ?? openSession(n.session_id, { newPane: true });
    if (!chat) return;
    const ref = `About the “${n.title}” note (${n.id}): `;
    const cur = getDraft(chat);
    if (!cur.includes(ref)) setDraft(chat, cur.trim() ? `${cur.replace(/\s+$/, "")}\n${ref}` : ref);
    focusComposer(chat);
  };

  /** Keep it from agents and bots (only chats it is linked in, and bots that have it as knowledge, read it), or not. */
  const setKept = async (keep: boolean) => {
    const n = note();
    if (!n || !!n.keep_from_agents === keep) return;
    try {
      const saved = await api.setNoteAgents(n.id, keep);
      const cur = note();
      if (cur?.id === saved.id) setNote({ ...cur, keep_from_agents: saved.keep_from_agents, all_bots: saved.all_bots });
      say(keep ? "Kept from agents: only chats you link it in can read it" : "Agents and bots can read it again");
    } catch (e) {
      toastError(e, "Couldn't change who can read it");
    }
  };

  /** An older server: which bots may read it, every bot or only bots it is given to (and chats it is brought into). */
  const setBots = async (all: boolean) => {
    const n = note();
    if (!n || !!n.all_bots === all) return;
    try {
      const saved = await api.setNoteBots(n.id, all);
      const cur = note();
      if (cur?.id === saved.id) setNote({ ...cur, all_bots: saved.all_bots });
      say(all ? "All your bots can read it now" : "Only bots you add can read it now");
    } catch (e) {
      toastError(e, "Couldn't change which bots can read it");
    }
  };

  const remove = async () => {
    const n = note();
    if (!n) return;
    const ok = await confirmDialog({ title: "Delete this note?", body: `“${n.title}” is removed for good, on every device.`, confirmLabel: "Delete note", danger: true });
    if (!ok) return;
    try {
      await api.deleteNote(n.id);
    } catch (e) {
      toastError(e, "Couldn't delete the note");
    }
  };

  const markdownOut = () => {
    const t = serverTitle(title());
    // The title as a heading, unless the text starts with it already.
    const first = text().trimStart().split("\n")[0].replace(/^#+\s*/, "").trim();
    return first === t ? text() : `# ${t}\n\n${text()}`;
  };
  const copyMarkdown = async () => {
    if (await copyText(markdownOut())) toast({ level: "success", message: "Copied the note as Markdown" });
  };
  /** The note as a file: its markdown, or (with pictures) a .zip of it and an assets/ folder, as the markdown refers to them. */
  const download = async () => {
    const name = serverTitle(title());
    const md = markdownOut();
    const pics = assetRefs(md);
    if (!pics.length) return void saveText(noteFileName(name, "md"), "text/markdown", md);
    try {
      const files = [{ path: noteFileName(name, "md"), data: new TextEncoder().encode(md) }];
      for (const p of pics) {
        try {
          files.push({ path: `assets/${p}`, data: new Uint8Array(await (await assetBytes(p)).arrayBuffer()) });
        } catch {
          /* gone from the server: the note still refers to it */
        }
      }
      await saveBlob(noteFileName(name, "zip"), "application/zip", zip(files));
    } catch (e) {
      toastError(e, "Couldn't make the download");
    }
  };
  const hasPictures = () => assetRefs(text()).length > 0;
  const outline = () => doc()?.headings() ?? [];

  const menu = (anchor: HTMLElement) => {
    const n = note();
    const items: MenuItem[] = [];
    if (n?.session_id) {
      items.push({ label: "Ask about this", icon: "chat", description: "Mention it in its chat's message box", onSelect: askAbout });
      items.push({ label: "Go to the chat", icon: "arrowRight", onSelect: () => openSession(n.session_id!, { newPane: true }) });
    }
    if (items.length) items.push({ separator: true });
    items.push({ label: "Find in note", icon: "search", hint: workspace.mode() === "tile" ? "Ctrl F" : undefined, onSelect: () => doc()?.find() });
    if (outline().length >= 2) items.push({ label: "Outline", icon: "menu", description: "Its headings, to jump to", onSelect: () => setView({ kind: "outline" }) });
    items.push({ separator: true });
    items.push({ label: "Read only", icon: "lock", checked: readOnly(), description: readOnly() ? "Tap to write in it again" : "Look without changing it by accident", onSelect: () => setReadOnly(noteId(), !readOnly()) });
    if (review()) items.push({ label: `Changes by ${review()!.who}`, icon: "eye", onSelect: () => setView({ kind: "review" }) });
    items.push({ label: "Version history", icon: "clock", onSelect: () => void openHistory() });
    // Agents and bots read every note unless the user keeps it from them.
    if (n && n.keep_from_agents !== undefined) {
      items.push({ separator: true });
      items.push({
        label: "Keep from agents",
        icon: "eyeOff",
        checked: n.keep_from_agents,
        description: n.keep_from_agents ? "Only chats you link it in can read it" : "Agents and bots can read it now",
        onSelect: () => void setKept(!n.keep_from_agents),
      });
    } else if (n && n.all_bots !== undefined && state.bots.some((b) => b.own_memory)) {
      // An older server: bots with their own memory read only what they are given; the user can open a note to all of them.
      items.push({ separator: true });
      items.push({ heading: "Bots that can read this note" });
      items.push({ label: "Only bots I add", checked: !n.all_bots, description: "Bots you give it to, and chats you bring it into", onSelect: () => void setBots(false) });
      items.push({ label: "All my bots", checked: n.all_bots, description: "Every bot can read it", onSelect: () => void setBots(true) });
    }
    items.push({ separator: true });
    items.push({ label: "Copy as Markdown", icon: "copy", onSelect: () => void copyMarkdown() });
    items.push(
      hasPictures()
        ? { label: "Download (.zip)", icon: "download", description: "The note and its pictures", onSelect: () => void download() }
        : { label: "Download (.md)", icon: "download", onSelect: () => void download() },
    );
    items.push({ separator: true });
    items.push({ label: "Close tile", icon: "x", hint: workspace.mode() === "tile" ? `${ALT} W` : undefined, onSelect: () => closePane(props.paneId) });
    if (n) items.push({ label: "Delete note…", icon: "trash", danger: true, description: "Removes it everywhere", onSelect: () => void remove() });
    openMenu(anchor, items, "bottom-end");
  };

  // A new, empty note starts on its title.
  const fresh = createMemo(() => !!note() && note()!.version === 1 && !note()!.content && note()!.title === "Untitled note");
  createEffect(
    on(fresh, (f) => {
      if (f) requestAnimationFrame(() => titleInput?.focus({ preventScroll: true }));
    }),
  );

  return (
    <div class="board note-tile" classList={{ "read-only": readOnly() }}>
      <header
        class="pane-head board-head"
        onPointerDown={(e) => {
          if ((e.target as HTMLElement).closest("button, input, a, [data-nodrag]")) return;
          pane.startDrag(e);
        }}
      >
        <Show when={pane.showMenuButton()}>
          <button type="button" class="icon-btn head-menu" data-keep-focus aria-label="Open sidebar" onClick={() => setDrawerOpen(true)}>
            <Icon name="menu" />
          </button>
        </Show>
        <div class="head-title">
          <span class="board-kind" aria-hidden="true">
            <Icon name="note" size={15} />
          </span>
          <div class="title-stack">
            <input
              ref={titleInput}
              class="note-title"
              value={title()}
              placeholder="Untitled note"
              aria-label="Note title"
              maxLength={120}
              disabled={!note()}
              readOnly={readOnly()}
              onInput={(e) => {
                setTitle(e.currentTarget.value);
                edited();
              }}
              onFocus={(e) => {
                if (e.currentTarget.value === "Untitled note" && !readOnly()) e.currentTarget.select();
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" || (e.key === "ArrowDown" && !e.shiftKey)) {
                  e.preventDefault();
                  if (!readOnly()) doc()?.focus("start");
                }
              }}
            />
            <span class="head-sub board-sub">
              <span>Note</span>
              <Show when={note()}>
                <span class="dot-sep" aria-hidden="true">
                  ·
                </span>
                <span class="note-by">{by()}</span>
                <span class="dot-sep" aria-hidden="true">
                  ·
                </span>
                <Show when={readOnly()} fallback={<span class="note-status" data-state={notice() ? "notice" : save()}>{status()}</span>}>
                  <button type="button" class="note-ro" data-tip="Write in it again" onClick={() => setReadOnly(noteId(), false)}>
                    <Icon name="lock" size={10} /> Read only
                  </button>
                </Show>
              </Show>
            </span>
          </div>
        </div>
        <div class="head-controls">
          {/* Computers: a picture in one click (phones have it on the format bar while writing). */}
          <Show when={note() && !readOnly() && !touchUi() && view().kind === "doc"}>
            <button type="button" class="icon-btn note-add-pic" aria-label="Add a picture" data-tip="Add a picture (or paste or drop one)" onClick={(e) => doc()?.pickPicture(e.currentTarget)}>
              <Icon name="image" size={16} />
            </button>
          </Show>
          <button type="button" class="icon-btn" aria-label="Note options" aria-haspopup="menu" data-tip="More" onClick={(e) => menu(e.currentTarget)}>
            <Icon name="more" size={17} />
          </button>
          <RestoreButton />
          {/* Phones: New at the top right; Close is in the ⋯ menu. */}
          <Show
            when={newInHeader() && pane.showMenuButton()}
            fallback={
              <button type="button" class="icon-btn head-close" aria-label="Close tile" data-tip="Close tile (the note stays)" data-kbd={`${ALT} W`} onClick={() => closePane(props.paneId)}>
                <Icon name="x" size={15} />
              </button>
            }
          >
            <NewTileButton />
          </Show>
        </div>
      </header>
      <Show when={recovered() && unsaved()}>
        <div class="note-bar" role="status">
          <span class="note-bar-text">Recovered unsaved changes from the closed tile.</span>
          <button type="button" class="btn btn-sm" onClick={() => {
            const n = theirs() ?? note();
            if (!n) return;
            saveSoon.cancel();
            discardNoteDraft(draftServer, n.id);
            setTheirs(null);
            setNote(n);
            setTitle(n.title);
            setText(n.content);
            setSave("saved");
            setRecovered(false);
          }}>Discard draft</button>
        </div>
      </Show>
      <Show when={theirs()}>
        {(t) => (
          <div class="note-bar warn" role="alert">
            <Icon name="alert" size={14} />
            <span class="note-bar-text">
              {whoElse(t())} changed the same part of this note while you were writing.
            </span>
            <span class="note-bar-actions">
              <button type="button" class="btn btn-xs btn-ghost" aria-pressed={view().kind === "compare"} onClick={() => setView(view().kind === "compare" ? { kind: "doc" } : { kind: "compare" })}>
                Compare
              </button>
              <button type="button" class="btn btn-xs" onClick={keepMine}>
                Keep mine
              </button>
              <button type="button" class="btn btn-xs btn-ghost" onClick={useTheirs}>
                Use theirs
              </button>
            </span>
          </div>
        )}
      </Show>
      <Show when={!theirs() && review() && view().kind === "doc"}>
        <div class="note-bar">
          <Icon name="sparkles" size={14} />
          <span class="note-bar-text">{review()!.who === "the agent" ? "The agent" : review()!.who} changed this note.</span>
          <span class="note-bar-actions">
            <button type="button" class="btn btn-xs btn-ghost" onClick={() => setView({ kind: "review" })}>
              See changes
            </button>
            <Show when={!readOnly()}>
              <button type="button" class="btn btn-xs btn-ghost" onClick={undoAgent}>
                Undo
              </button>
            </Show>
            <button type="button" class="icon-btn sm" aria-label="Dismiss" data-tip="Keep the changes" onClick={doneReview}>
              <Icon name="x" size={12} />
            </button>
          </span>
        </div>
      </Show>
      <div class="board-body note-body">
        <Show
          when={note()}
          fallback={
            <div class="board-loading">
              <Show when={missing()} fallback={
                <Show when={loadFailed()} fallback={<Tessellate size={10} />}>
                  <span>Couldn't open the note.</span>
                  <button type="button" class="btn btn-sm" onClick={() => void open(noteId())}>Retry</button>
                </Show>
              }>
                <Icon name="note" size={20} />
                <span>This note is gone.</span>
                <button type="button" class="btn btn-sm" onClick={() => closePane(props.paneId)}>
                  Close tile
                </button>
              </Show>
            </div>
          }
        >
          {/* The page stays put under the panels (its caret, scroll and undo with it). */}
          <Suspense fallback={<div class="board-loading"><Tessellate size={10} /></div>}>
            <NoteEditor
              noteId={noteId()}
              text={text()}
              readOnly={readOnly()}
              visible={() => pane.visible() && view().kind === "doc"}
              backlinks={links}
              onChange={(t) => {
                setText(t);
                edited();
              }}
              onSave={() => saveSoon.flush()}
              handle={setDoc}
            />
          </Suspense>
          <Switch>
            <Match when={view().kind === "review" && review()}>
              {(r) => (
                <Panel
                  title={`Changes by ${r().who}`}
                  onBack={() => setView({ kind: "doc" })}
                  actions={
                    <>
                      <Show when={!readOnly()}>
                        <button type="button" class="btn btn-xs btn-ghost" onClick={undoAgent}>
                          Undo them
                        </button>
                      </Show>
                      <button type="button" class="btn btn-xs" onClick={doneReview}>
                        Keep
                      </button>
                    </>
                  }
                >
                  <NoteDiff before={r().before} after={r().after} />
                </Panel>
              )}
            </Match>
            <Match when={view().kind === "compare" && theirs()}>
              {(t) => (
                <Panel title={`Their version → yours`} onBack={() => setView({ kind: "doc" })}>
                  <NoteDiff before={t().content} after={text()} />
                </Panel>
              )}
            </Match>
            <Match when={view().kind === "history"}>
              <Panel title="Version history" onBack={() => setView({ kind: "doc" })}>
                <Show when={history()} fallback={<div class="board-loading"><Tessellate size={10} /></div>}>
                  {(list) => (
                    <ul class="nh-list">
                      <li class="nh-row now">
                        <span class="nh-when">Now</span>
                        <span class="nh-by">{note()!.agent_edit ? agentName(note()!.edit_session) : "You"}</span>
                      </li>
                      <For each={list()}>
                        {(v) => (
                          <li>
                            <button type="button" class="nh-row" onClick={() => void showVersion(v)}>
                              <span class="nh-when" title={new Date(v.updated_at).toLocaleString()}>
                                {relTime(v.updated_at)}
                              </span>
                              <span class="nh-by">{versionBy(v)}</span>
                              <Icon name="chevronRight" size={13} />
                            </button>
                          </li>
                        )}
                      </For>
                      <Show when={!list().length}>
                        <li class="nh-empty">Earlier versions show up here once the note changes: one before each of an agent's edits, and one every few minutes while you write.</li>
                      </Show>
                    </ul>
                  )}
                </Show>
              </Panel>
            </Match>
            <Match when={(() => { const v = view(); return v.kind === "version" ? v.v : null; })()}>
              {(v) => (
                <Panel
                  title={`${relTime(v().updated_at)} · ${versionBy(v())}`}
                  sub="What changed since then"
                  onBack={() => void openHistory()}
                  actions={
                    <Show when={!readOnly()}>
                      <button type="button" class="btn btn-xs" onClick={() => restore(v())}>
                        Restore this version
                      </button>
                    </Show>
                  }
                >
                  <NoteDiff before={v().content} after={text()} />
                </Panel>
              )}
            </Match>
            <Match when={view().kind === "outline"}>
              <Panel title="Outline" onBack={() => setView({ kind: "doc" })}>
                <ul class="nh-list note-outline">
                  <For each={outline()}>
                    {(h) => (
                      <li>
                        <button
                          type="button"
                          class="nh-row"
                          style={{ "padding-left": `${10 + (h.level - 1) * 14}px` }}
                          data-level={h.level}
                          onClick={() => {
                            setView({ kind: "doc" });
                            requestAnimationFrame(() => doc()?.goTo(h.pos));
                          }}
                        >
                          <span class="nh-when">{h.text}</span>
                        </button>
                      </li>
                    )}
                  </For>
                </ul>
              </Panel>
            </Match>
          </Switch>
        </Show>
      </div>
    </div>
  );
}

/** A view over the note (changes, history): a slim bar with Back, then its content. */
function Panel(props: { title: string; sub?: string; onBack: () => void; actions?: import("solid-js").JSX.Element; children: import("solid-js").JSX.Element }) {
  return (
    <div class="note-panel">
      <div class="np-bar">
        <button type="button" class="icon-btn sm" aria-label="Back to the note" data-tip="Back" onClick={() => props.onBack()}>
          <Icon name="arrowLeft" size={14} />
        </button>
        <span class="np-title">
          {props.title}
          <Show when={props.sub}>
            <span class="np-sub"> · {props.sub}</span>
          </Show>
        </span>
        <span class="np-actions">{props.actions}</span>
      </div>
      <div class="np-body">{props.children}</div>
    </div>
  );
}
