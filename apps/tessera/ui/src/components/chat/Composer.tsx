import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on, onCleanup, onMount, untrack } from "solid-js";
import type { ContentPart } from "../../proto/ContentPart";
import type { Delivery } from "../../proto/Delivery";
import type { Bot } from "../../proto/Bot";
import {
  abortSession,
  chooseCwd,
  compactSession,
  fastAvailable,
  forkAndOpen,
  newPrivateChat,
  paneSettings,
  parsePermission,
  patchPane,
  renameSession,
  setPaneFast,
  setPanePermission,
  setSessionGoal,
  startChat,
  toggleYolo,
} from "../../lib/actions";
import { paneAction } from "../../lib/bus";
import {
  MAX_FILES,
  MAX_FILE_BYTES,
  MAX_PRIVATE_TEXT,
  attachmentToPart,
  fileToAttachment,
  fileToFileAttachment,
  humanBytes,
  isTextFile,
  pastedTextAttachment,
  type Attachment,
} from "../../lib/images";
import { clipboardFiles, dropMayHide, droppedFiles, insertedChunk, isBigPaste, lineCount, onReceivedFiles, pasteMayHide, transferFiles } from "../../lib/clipboard";
import { FileIcon } from "./Messages";
import { prefs } from "../../lib/prefs";
import { rewindLast } from "../../lib/rewind";
import { botById, botByHandle, findModel, isBusy, isRoom, sendMessage, state, type SessionState } from "../../lib/store";
import { parseThinking, shortLabel, thinkingLabel } from "../../lib/thinking";
import { offlineLate, openSettings, toast, toastError } from "../../lib/ui";
import { spawnTile } from "../../lib/spawn";
import { checkBoardUrl, normalizeUrl, pageTitle, showBesideChat } from "../../lib/boards";
import { coarsePointer, reducedMotion, touchUi } from "../../lib/util";
import { useVoice } from "../../lib/voice-host";
import { PartError } from "../PartError";
import {
  composerAttachments,
  setComposerAttachments,
  prepareAttachments,
  focusComposer,
  draftIdentity,
  getDraft,
  isPrivatePane,
  newChat,
  openBot,
  registerComposer,
  setDraft,
  splitPane,
  stagedAttachments,
  takeStagedAttachments,
  workspace,
} from "../../lib/workspace";
import { BotAvatar, botColor } from "../bots";
import { Icon, Spinner } from "../icons";
import { openMenu } from "../primitives";
import { QueueCards } from "./QueueCards";

interface SlashCommand {
  name: string;
  args?: string;
  desc: string;
}

export const COMMANDS: SlashCommand[] = [
  { name: "new", desc: "New chat in a new tile" },
  { name: "private", desc: "New private chat: nothing is saved" },
  { name: "clear", desc: "Start a new chat in this tile" },
  { name: "steer", args: "<message>", desc: "Redirect the working agent now: it reads this at its next step" },
  { name: "queue", args: "<message>", desc: "Queue for when the agent is done (what Enter does while it works)" },
  { name: "goal", args: "[goal | clear]", desc: "Set the goal of this chat; alone it shows it, “clear” removes it" },
  { name: "compact", args: "[instructions]", desc: "Summarize older context to free up room" },
  { name: "model", args: "[provider/model]", desc: "Switch the model" },
  { name: "thinking", args: "[level]", desc: "How hard the model thinks (options depend on the model)" },
  { name: "permission", args: "[default|ask|yolo|read_only]", desc: "Set the permission mode" },
  { name: "yolo", desc: "Toggle YOLO — never ask for approval" },
  { name: "fast", args: "[on|off]", desc: "Fast mode: quicker replies, uses more credits" },
  { name: "show", args: "[address]", desc: "Show a web page beside this chat (alone: an empty one to type in)" },
  { name: "split", desc: "Open a new tile beside this one" },
  { name: "bot", args: "[handle]", desc: "Chat with a bot in a new tile" },
  { name: "cwd", args: "[path]", desc: "Change the working directory" },
  { name: "fork", desc: "Branch this chat into a new tile" },
  { name: "rewind", desc: "Go back to before your last message: undo its changes and edit it" },
  { name: "rename", args: "[title]", desc: "Rename this chat" },
];

export function Composer(props: { paneId: string; session?: SessionState; hero?: boolean }) {
  const text = () => getDraft(props.paneId);
  const setText = (t: string) => setDraft(props.paneId, t);
  const atts = () => composerAttachments(draftIdentity(props.paneId));
  const setAtts = (next: Attachment[] | ((list: Attachment[]) => Attachment[])) => setComposerAttachments(draftIdentity(props.paneId), next);
  const [sending, setSending] = createSignal(false);
  const [dragOver, setDragOver] = createSignal(false);
  const [focused, setFocused] = createSignal(false);
  const [slashIdx, setSlashIdx] = createSignal(0);
  const [slashDismissed, setSlashDismissed] = createSignal(false);
  const [caret, setCaret] = createSignal(0);
  const [mentionIdx, setMentionIdx] = createSignal(0);
  const [mentionDismissed, setMentionDismissed] = createSignal(false);
  /** The "Pasted text" attachment shown in full above the box. */
  const [peek, setPeek] = createSignal<string | null>(null);
  let ta!: HTMLTextAreaElement;
  let field!: HTMLDivElement;
  let fileInput!: HTMLInputElement;
  /** Phones and tablets only: opens the camera app. */
  let cameraInput!: HTMLInputElement;

  const busy = () => isBusy(props.session?.info);
  const touch = touchUi();
  const hasContent = () => !!text().trim() || atts().length > 0;
  const model = () => findModel(paneSettings(props.paneId).model);

  onMount(() => {
    const off = registerComposer(
      props.paneId,
      () => ta?.focus({ preventScroll: true }),
      // Typing over the tile with nothing focused: the key lands here, at the caret.
      (t) => {
        if (!ta || voice() !== "idle") return;
        ta.focus({ preventScroll: true });
        ta.setRangeText(t, ta.selectionStart, ta.selectionEnd, "end");
        setText(ta.value);
        syncCaret();
      },
    );
    onCleanup(off);
    // Images an Android keyboard inserted (its clipboard, GIFs, stickers): for the box being typed in.
    onCleanup(
      onReceivedFiles((files) => {
        if (!ta || document.activeElement !== ta) return false;
        void addFiles(files);
        return true;
      }),
    );
    resize(true);
    if (!coarsePointer() && props.hero) requestAnimationFrame(() => ta.focus({ preventScroll: true }));
    // The tile or the on-screen keyboard changing size moves the line breaks and the limit.
    const ro = new ResizeObserver(() => resize(true));
    ro.observe(field);
    const vv = window.visualViewport;
    const onVv = () => resize(true);
    vv?.addEventListener("resize", onVv);
    onCleanup(() => {
      ro.disconnect();
      vv?.removeEventListener("resize", onVv);
    });
  });

  /**
   * Grow with the text, up to 40% of what's visible (the on-screen keyboard
   * included), then scroll inside. The textarea takes its new height at once,
   * so its text never scrolls while typing; only the frame around it eases to
   * the new size (it clips for a moment instead of jumping).
   */
  let lastH = 0;
  let lastW = 0;
  const resize = (instant = false) => {
    if (!ta || !field) return;
    const view = window.visualViewport?.height ?? window.innerHeight;
    const max = Math.max(96, Math.round(view * 0.4));
    const top = ta.scrollTop;
    ta.style.height = "0px";
    const need = ta.scrollHeight;
    const h = Math.min(need, max);
    ta.style.height = `${h}px`;
    ta.style.overflowY = need > max ? "auto" : "hidden";
    ta.scrollTop = need > max ? top : 0;
    const w = ta.clientWidth;
    if (h === lastH && w === lastW) return;
    const grow = !instant && lastH > 0 && w === lastW && !reducedMotion();
    lastH = h;
    lastW = w;
    field.style.transition = grow ? "" : "none";
    field.style.height = `${h}px`;
    if (!grow) {
      void field.offsetHeight;
      field.style.transition = "";
    }
  };
  // Attachments handed over with a draft (a rewound message's images).
  createEffect(() => {
    if (!stagedAttachments(props.paneId).length) return;
    const staged = untrack(() => takeStagedAttachments(props.paneId));
    setAtts((l) => [...l, ...staged].slice(0, MAX_FILES));
  });
  createEffect(on(text, () => resize()));
  createEffect(on(text, () => setSlashDismissed(false), { defer: true }));

  const slashMatches = createMemo(() => {
    const m = text().match(/^\/(\S*)$/);
    if (!m || slashDismissed()) return [];
    const q = m[1].toLowerCase();
    // /fast only where the model offers it (typing it elsewhere still explains).
    return COMMANDS.filter((c) => c.name.startsWith(q) && (c.name !== "fast" || fastAvailable(props.paneId)));
  });
  createEffect(on(slashMatches, () => setSlashIdx(0)));

  // @mentions: channel members in rooms, any bot elsewhere.
  const info = () => props.session?.info;
  const mentionPool = (): Bot[] => {
    const i = info();
    if (i && isRoom(i)) return i.members.map((id) => botById(id)).filter((b): b is Bot => !!b);
    return state.bots;
  };
  const mentionQuery = createMemo(() => {
    const m = /(^|[\s(])@([a-z0-9_-]*)$/i.exec(text().slice(0, caret()));
    return m ? m[2].toLowerCase() : null;
  });
  const mentionMatches = createMemo(() => {
    const q = mentionQuery();
    if (q === null || mentionDismissed()) return [];
    return mentionPool()
      .filter((b) => b.handle.toLowerCase().startsWith(q) || b.name.toLowerCase().startsWith(q))
      .slice(0, 6);
  });
  createEffect(on(mentionMatches, () => setMentionIdx(0)));
  createEffect(on(text, () => setMentionDismissed(false), { defer: true }));
  const syncCaret = () => setCaret(ta?.selectionStart ?? text().length);
  const completeMention = (b: Bot) => {
    const before = text().slice(0, caret());
    const after = text().slice(caret());
    const replaced = before.replace(/@([a-z0-9_-]*)$/i, `@${b.handle} `);
    setText(replaced + after);
    queueMicrotask(() => {
      ta.focus();
      ta.setSelectionRange(replaced.length, replaced.length);
      setCaret(replaced.length);
    });
  };

  const priv = () => isPrivatePane(props.paneId);
  /**
   * A big paste as a "Pasted text" attachment, sent as a text file the agent
   * reads in full. False when it can't be one (then it is pasted as text).
   */
  const addPastedText = (text: string): boolean => {
    if (atts().length >= MAX_FILES) return false;
    const bytes = new TextEncoder().encode(text).length;
    if (bytes > (priv() ? MAX_PRIVATE_TEXT : MAX_FILE_BYTES)) return false;
    const n = Math.max(0, ...atts().map((a) => (a.pasted === undefined ? 0 : Number(/ (\d+)\.txt$/.exec(a.name)?.[1] ?? 1)))) + 1;
    setAtts((l) => [...l, pastedTextAttachment(text, n)]);
    return true;
  };
  const removeAtt = (a: Attachment) => {
    setAtts((l) => l.filter((x) => x.id !== a.id));
    if (peek() === a.id) setPeek(null);
  };
  /** Put a "Pasted text" back into the message, at the caret. */
  const expandPasted = (a: Attachment) => {
    removeAtt(a);
    const t = a.pasted ?? "";
    const at = ta?.selectionStart ?? text().length;
    const end = ta?.selectionEnd ?? at;
    const cur = text();
    setText(cur.slice(0, at) + t + cur.slice(end));
    queueMicrotask(() => {
      ta?.focus();
      ta?.setSelectionRange(at + t.length, at + t.length);
      syncCaret();
    });
  };
  /** Any file: images go in as images (downscaled), everything else as an attachment. */
  const addFiles = async (files: File[]) => {
    const identity = draftIdentity(props.paneId);
    const preparation = prepareAttachments(identity); // Register before the first decode/read.
    try { for (const f of files) {
      if (atts().length >= MAX_FILES) {
        toast({ level: "warning", message: `Attach at most ${MAX_FILES} files at a time.` });
        return;
      }
      const image = /^image\/(png|jpeg|gif|webp|bmp|avif|heic|heif)$/.test(f.type);
      if (priv() && !image && !(isTextFile({ name: f.name, mime: f.type }) && f.size <= MAX_PRIVATE_TEXT)) {
        toast({ level: "warning", title: f.name, message: "Private chats take text files and images only, because nothing from them is written to disk." });
        continue;
      }
      try {
        const a = image ? await fileToAttachment(f) : await fileToFileAttachment(f);
        preparation.append(a);
      } catch (e) {
        toastError(e, "Could not attach file");
      }
    } } finally { preparation.finish(); }
  };

  // ChatPane owns voice so it survives the first send replacing the hero.
  const voiceHost = useVoice();
  const voice = () => voiceHost.view().phase;
  const voiceOn = () => state.server?.voice_engine ? state.server.voice_engine !== "off" : !!state.server?.voice;
  const startVoice = (conversation = false) => voiceHost.start(conversation, [ta?.selectionStart ?? text().length, ta?.selectionEnd ?? text().length]);
  let pressedAt = 0;
  let releasePending = false;
  let beganDictation = false;
  const micDown = (e: PointerEvent & { currentTarget: HTMLButtonElement }) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    pressedAt = Date.now(); releasePending = false;
    beganDictation = voice() === "idle";
    if (beganDictation) void startVoice().then(() => { if (releasePending) voiceHost.finish(true); });
  };
  const micUp = () => {
    if (!pressedAt) return;
    const held = Date.now() - pressedAt > 350; pressedAt = 0;
    if (held) { releasePending = true; voiceHost.finish(true); }
    else if (!beganDictation) voiceHost.finish(prefs.voiceAutoSend);
  };

  const runSlash = async (line: string): Promise<boolean> => {
    const m = line.match(/^\/(\w+)\s*([\s\S]*)$/);
    if (!m) return false;
    const [, name, rawArgs] = m;
    const args = rawArgs.trim();
    const sid = props.session?.info.id;
    const needSession = () => {
      toast({ level: "info", message: "Send a message first — this chat has no history yet." });
      return true;
    };
    switch (name.toLowerCase()) {
      case "new":
        newChat({ newPane: true });
        return true;
      case "clear":
        newChat({ pane: props.paneId });
        return true;
      case "private":
        newPrivateChat();
        return true;
      case "steer":
      case "queue": {
        if (!args) {
          toast({ level: "info", message: `Use /${name.toLowerCase()} followed by your message.` });
          return true;
        }
        if (!props.session) {
          setSending(true);
          try {
            await startChat(props.paneId, [{ type: "text", text: args }]);
          } finally {
            setSending(false);
          }
          return true;
        }
        await sendMessage(props.session.info.id, [{ type: "text", text: args }], busy() ? (name.toLowerCase() === "steer" ? "steer" : "follow_up") : "auto");
        return true;
      }
      case "goal": {
        if (!sid) return needSession();
        // The goal bar at the top of the chat shows it once set.
        if (args) {
          await setSessionGoal(sid, /^(clear|none|off|-)$/i.test(args) ? "" : args);
          return true;
        }
        const g = props.session?.info.task?.goal?.trim();
        toast(g ? { level: "info", title: "Goal", message: g, timeout: 6000 } : { level: "info", message: "No goal yet. Type /goal and what this chat should achieve.", timeout: 4000 });
        return true;
      }
      case "compact":
        if (!sid) return needSession();
        await compactSession(sid, args || undefined);
        return true;
      case "model": {
        if (args) {
          const [provider, ...rest] = args.split("/");
          if (!provider || !rest.length) {
            toast({ level: "warning", message: "Use /model provider/model" });
            return true;
          }
          await patchPane(props.paneId, { model: { provider, model: rest.join("/") } });
        } else paneAction(props.paneId, "model");
        return true;
      }
      case "thinking": {
        const m = model();
        const v = args ? parseThinking(args, m) : null;
        const opts = m?.thinking ?? [];
        if (v && (!opts.length || opts.some((o) => o.level === v))) {
          await patchPane(props.paneId, { thinking: v });
          toast({ level: "info", message: `Thinking: ${shortLabel(thinkingLabel(v, m))}`, timeout: 1600 });
        } else if (args && m && !opts.length) toast({ level: "info", message: `${m.name} has no thinking modes.` });
        else if (args && v) toast({ level: "info", message: `${m?.name ?? "This model"} supports: ${opts.map((o) => shortLabel(o.label)).join(", ")}` });
        else paneAction(props.paneId, "thinking");
        return true;
      }
      case "permission":
      case "permissions": {
        const v = parsePermission(args);
        if (v) await setPanePermission(props.paneId, v);
        else paneAction(props.paneId, "permission");
        return true;
      }
      case "yolo":
        await toggleYolo(props.paneId);
        return true;
      case "fast": {
        const on = /^(on|yes|1|true)$/i.test(args) ? true : /^(off|no|0|false)$/i.test(args) ? false : !paneSettings(props.paneId).fast;
        await setPaneFast(props.paneId, on);
        return true;
      }
      case "show": {
        if (!sid) return needSession();
        if (priv()) {
          toast({ level: "info", message: "Private chats can't show whiteboards." });
          return true;
        }
        if (!args) {
          // An empty web page tile beside this one: type the address there.
          spawnTile("url");
          return true;
        }
        const c = checkBoardUrl(normalizeUrl(args));
        if (!c.ok) {
          toast({ level: "warning", message: c.reason });
          return true;
        }
        await showBesideChat(sid, pageTitle(c.url.toString()), { type: "url", url: c.url.toString() });
        return true;
      }
      case "split":
        splitPane("right", props.paneId);
        return true;
      case "bot": {
        const b = args ? botByHandle(args) : undefined;
        if (b) openBot(b.id, { newPane: true });
        else toast({ level: "info", message: args ? `No bot called @${args.replace(/^@/, "")}` : "Use /bot handle — or type @ in the command palette." });
        return true;
      }
      case "cwd":
        if (args) await patchPane(props.paneId, { cwd: args });
        else await chooseCwd(props.paneId);
        return true;
      case "fork":
        if (!sid) return needSession();
        await forkAndOpen(sid);
        return true;
      case "rewind":
      case "undo":
        rewindLast(sid, props.paneId);
        return true;
      case "rename":
        if (!sid) return needSession();
        if (args) await renameSession(sid, args);
        else paneAction(props.paneId, "rename");
        return true;
      default:
        return false;
    }
  };

  const submit = async (forced?: "steer" | "follow_up" | "interrupt", fromVoice = false) => {
    if (!fromVoice && voice() !== "idle") {
      if (voice() === "listening") voiceHost.finish(false);
      else voiceHost.stop();
      return;
    }
    const identity = draftIdentity(props.paneId);
    const session = props.session;
    const raw = text();
    const trimmed = raw.trim();
    if (trimmed.startsWith("/") && !atts().length && !/^\/\S*\//.test(trimmed)) {
      if (await runSlash(trimmed)) {
        setDraft(props.paneId, "", identity);
        return;
      }
    }
    if (identity !== draftIdentity(props.paneId) || !hasContent() || sending()) return;
    const files = atts();
    const content: ContentPart[] = files.filter((a) => a.kind === "image").map(attachmentToPart);
    const attachments = files.filter((a) => a.kind === "file").map((a) => ({ name: a.name, mime: a.mime, data: a.data }));
    if (trimmed) content.push({ type: "text", text: raw.replace(/\s+$/, "").replace(/^\n+/, "") });
    // While the agent works a plain send waits in the queue (the server decides:
    // `auto` queues a follow-up); /steer or Ctrl+Enter steers, Ctrl+Shift+Enter
    // stops the agent and sends it now.
    const delivery: Delivery = forced ?? "auto";
    setText("");
    setAtts([]);
    try {
      if (!session) {
        setSending(true);
        await startChat(props.paneId, content, attachments);
      } else {
        await sendMessage(session.info.id, content, delivery, attachments);
      }
    } catch (e) {
      if (!fromVoice) toastError(e, "Message not sent");
      if (!getDraft(props.paneId, identity)) setDraft(props.paneId, raw, identity);
      if (identity === draftIdentity(props.paneId) && !atts().length) setAtts(files);
      if (fromVoice) throw e;
    } finally {
      setSending(false);
      // The hero composer is replaced by the docked one after the first send.
      focusComposer(props.paneId);
    }
  };
  onCleanup(voiceHost.bindSend(async (words) => { setText(words); await submit("interrupt", true); }));

  const completeSlash = (c: SlashCommand) => {
    setText(`/${c.name}${c.args ? " " : ""}`);
    if (!c.args) setSlashDismissed(true);
    ta.focus();
  };

  const onKeyDown = (e: KeyboardEvent) => {
    const mentions = mentionMatches();
    if (mentions.length) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        setMentionIdx((i) => (i + (e.key === "ArrowDown" ? 1 : -1) + mentions.length) % mentions.length);
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && !e.altKey)) {
        e.preventDefault();
        completeMention(mentions[mentionIdx()]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setMentionDismissed(true);
        return;
      }
    }
    const matches = slashMatches();
    if (matches.length) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        setSlashIdx((i) => (i + (e.key === "ArrowDown" ? 1 : -1) + matches.length) % matches.length);
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        completeSlash(matches[slashIdx()]);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey && !e.altKey) {
        const c = matches[slashIdx()];
        const typed = text().slice(1).toLowerCase();
        if (typed !== c.name || c.args) {
          e.preventDefault();
          if (typed === c.name && c.args) void submit();
          else completeSlash(c);
          return;
        }
      }
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setSlashDismissed(true);
        return;
      }
    }
    const mod = e.ctrlKey || e.metaKey;
    // While the agent works: Ctrl+Enter steers it now, Ctrl+Shift+Enter stops
    // it and sends this now, Ctrl+Q queues (like Enter). When idle all simply send.
    if (mod && !e.altKey && (e.key === "Enter" || (e.code === "KeyQ" && !e.shiftKey)) && !e.isComposing) {
      e.preventDefault();
      e.stopPropagation();
      if (hasContent()) void submit(busy() ? (e.key !== "Enter" ? "follow_up" : e.shiftKey ? "interrupt" : "steer") : undefined);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      // Alt+Enter is always "new tile": let it reach the global shortcut.
      if (e.altKey) return;
      if (coarsePointer()) return; // newline on touch keyboards
      e.preventDefault();
      void submit();
    }
  };

  const onPaste = (e: ClipboardEvent) => {
    const dt = e.clipboardData;
    const files = transferFiles(dt);
    if (files.length) {
      e.preventDefault();
      void addFiles(files);
      return;
    }
    const pasted = dt?.getData("text/plain") ?? "";
    if (!pasteMayHide(dt, pasted)) {
      // Small pastes go in as typed; big ones become an attachment.
      if (pasted && isBigPaste(pasted) && addPastedText(pasted)) e.preventDefault();
      return;
    }
    // The desktop app's web view keeps copied files and images to itself: they
    // win over the link, markup or path beside them; else the text goes in.
    e.preventDefault();
    void clipboardFiles().then((found) => (found ? addFiles(found) : pasteText(pasted)));
  };
  /** Text a paste held after all, at the caret (as typed: the box keeps its undo history). */
  const pasteText = (t: string) => {
    if (!t || !ta || (isBigPaste(t) && addPastedText(t))) return;
    ta.focus({ preventScroll: true });
    if (!document.execCommand("insertText", false, t)) ta.setRangeText(t, ta.selectionStart, ta.selectionEnd, "end");
    setText(ta.value);
    syncCaret();
  };

  const onInput = (e: InputEvent & { currentTarget: HTMLTextAreaElement }) => {
    const el = e.currentTarget;
    // A keyboard's clipboard (Android) commits a big paste as typing: make it an attachment too.
    const chunk = e.inputType !== "insertCompositionText" ? insertedChunk(text(), el.value) : null;
    if (chunk && addPastedText(chunk.text)) {
      const rest = el.value.slice(0, chunk.start) + el.value.slice(chunk.start + chunk.text.length);
      el.value = rest;
      el.setSelectionRange(chunk.start, chunk.start);
      setText(rest);
    } else setText(el.value);
    syncCaret();
  };

  const draftBot = () => {
    const c = workspace.panes[props.paneId]?.content;
    return c?.type === "chat" && !c.sessionId ? botById(c.draft?.botId) : undefined;
  };
  const bot = () => (info()?.kind === "channel" ? undefined : botById(info()?.bot_id)) ?? draftBot();
  const placeholderBusy = () => (touch ? "Queue a follow-up…" : "Queue a follow-up · /steer to redirect now");
  const placeholder = () => {
    const i = info();
    if (offlineLate()) return "Offline · sends later";
    if (i && isRoom(i)) return busy() ? "Add to the conversation…" : `Message #${i.title || "channel"}`;
    if (busy()) return placeholderBusy();
    if (bot()) return `Message ${bot()!.name}…`;
    if (isPrivatePane(props.paneId)) return busy() ? placeholderBusy() : "Ask anything — nothing is saved";
    if (props.hero) return "Ask anything, or describe a task…";
    return "Message Tessera…";
  };

  /** Phones and tablets: a small menu next to the button (a computer has no camera to offer: its button opens the picker). */
  const openAttachMenu = (anchor: HTMLElement) =>
    openMenu(
      anchor,
      [
        { label: "Take photo", icon: "camera", onSelect: () => cameraInput.click() },
        { label: "Photos & files", icon: "image", onSelect: () => fileInput.click() },
      ],
      "top-start",
      { sheet: false, class: "attach-pop" },
    );
  /** What either input picked. The value is cleared so choosing again (a second photo) fires `change` again. */
  const picked = (input: HTMLInputElement) => {
    void addFiles(Array.from(input.files ?? []));
    input.value = "";
  };
  const attachButton = () => (
    <button
      type="button"
      class="icon-btn comp-btn"
      aria-label={touch ? "Attach a photo or file" : "Attach files"}
      aria-haspopup={touch ? "menu" : undefined}
      data-tip={priv() ? "Attach text files or images (private chats keep nothing on disk)" : "Attach files"}
      onClick={(e) => (touch ? openAttachMenu(e.currentTarget) : fileInput.click())}
    >
      <Icon name="paperclip" size={18} />
    </button>
  );
  const micButton = () => (
    <button
      type="button"
      class="icon-btn comp-btn mic-btn"
      classList={{ on: voice() === "listening" || voice() === "starting" }}
      aria-label={voice() === "idle" ? "Voice input" : "Finish recording"}
      aria-pressed={voice() !== "idle"}
      data-tip={voice() === "idle" ? "Tap to dictate · hold to send" : "Finish dictation"}
      onPointerDown={micDown}
      onPointerUp={micUp}
      onPointerCancel={() => { pressedAt = 0; voiceHost.stop(); }}
      onClick={(e) => {
        if (releasePending) { releasePending = false; return; }
        if (e.detail === 0 && voice() === "idle") void startVoice();
        else if (e.detail === 0 && voice() === "listening") voiceHost.finish(prefs.voiceAutoSend);
      }}
    >
      <Show when={voice() === "starting"} fallback={<Icon name={voice() === "listening" ? "stop" : "mic"} size={18} />}>
        <Spinner size={16} />
      </Show>
    </button>
  );
  const sendTip = () => (busy() ? (touch ? "Queue: sent when the agent is done" : "Queue (↵) · steer (Ctrl ↵) · send now (Ctrl ⇧ ↵)") : "Send");

  return (
    <div class="composer-wrap" classList={{ "in-hero": !!props.hero }}>
      <Show when={props.session?.queued.length}>
        <QueueCards session={props.session!} />
      </Show>
      <div
        class="composer"
        classList={{ focused: focused(), drag: dragOver(), busy: busy(), recording: voice() !== "idle", conversation: voice() !== "idle" && voiceHost.view().conversation, "has-atts": atts().length > 0 }}
        onDragOver={(e) => {
          if (e.dataTransfer?.types.includes("Files") || dropMayHide(e.dataTransfer)) {
            e.preventDefault();
            setDragOver(true);
          }
        }}
        onDragLeave={() => setDragOver(false)}
        onDrop={(e) => {
          setDragOver(false);
          const files = Array.from(e.dataTransfer?.files ?? []);
          if (files.length) {
            e.preventDefault();
            void addFiles(files);
          } else if (dropMayHide(e.dataTransfer)) {
            // From a file manager into the desktop app: the web view gives links, the shell the files.
            e.preventDefault();
            const link = e.dataTransfer?.getData("text/uri-list") || e.dataTransfer?.getData("text/plain") || "";
            void droppedFiles().then((found) => (found ? addFiles(found) : pasteText(link)));
          }
        }}
        onClick={(e) => {
          if (e.target === e.currentTarget || (e.target as HTMLElement).classList.contains("comp-row")) ta.focus();
        }}
      >
        <Show when={atts().length}>
          <div class="attachments" role="group" aria-label="Attachments">
            <For each={atts()}>
              {(a) => (
                <Switch
                  fallback={
                    <div class="file-chip pending" title={a.name}>
                      <FileIcon name={a.name} />
                      <div class="file-chip-info">
                        <span class="file-chip-name">{a.name}</span>
                        <span class="file-chip-size">{humanBytes(a.size)}</span>
                      </div>
                      <button type="button" class="file-chip-x" aria-label={`Remove ${a.name}`} onClick={() => removeAtt(a)}>
                        <Icon name="x" size={12} />
                      </button>
                    </div>
                  }
                >
                  <Match when={a.kind === "image"}>
                    <div class="attachment" title={a.name}>
                      <img src={a.preview} alt={a.name} />
                      <button type="button" class="attachment-x" aria-label={`Remove ${a.name}`} onClick={() => removeAtt(a)}>
                        <Icon name="x" size={12} />
                      </button>
                    </div>
                  </Match>
                  <Match when={a.pasted !== undefined}>
                    <div class="file-chip pending pasted-chip" classList={{ open: peek() === a.id }}>
                      <button
                        type="button"
                        class="pasted-open"
                        aria-expanded={peek() === a.id}
                        aria-label={`${pastedLabel(a)}: show it`}
                        onClick={() => setPeek(peek() === a.id ? null : a.id)}
                      >
                        <Icon name="file" size={15} class="file-icon" />
                        <span class="file-chip-name">{pastedLabel(a)}</span>
                        <span class="file-chip-size">
                          {lineCount(a.pasted!).toLocaleString()} lines · {humanBytes(a.size)}
                        </span>
                      </button>
                      <button type="button" class="file-chip-x" aria-label={`Remove ${pastedLabel(a)}`} onClick={() => removeAtt(a)}>
                        <Icon name="x" size={12} />
                      </button>
                    </div>
                  </Match>
                </Switch>
              )}
            </For>
            <Show when={model() && !model()!.vision && atts().some((a) => a.kind === "image")}>
              <span class="attach-warn">
                <Icon name="alert" size={13} /> {model()!.name} may not accept images
              </span>
            </Show>
          </div>
        </Show>
        <Show when={atts().find((a) => a.id === peek())}>
          {(a) => (
            <div class="pasted-peek" role="region" aria-label={pastedLabel(a())}>
              <pre class="pasted-peek-text">{peekText(a().pasted ?? "")}</pre>
              <div class="pasted-peek-bar">
                <span class="pasted-peek-note">Sent as a file the agent reads in full</span>
                <button type="button" class="btn btn-sm btn-ghost" onClick={() => expandPasted(a())}>
                  Put in message
                </button>
                <button type="button" class="btn btn-sm btn-ghost" onClick={() => removeAtt(a())}>
                  Remove
                </button>
                <button type="button" class="icon-btn sm" aria-label="Close the preview" onClick={() => setPeek(null)}>
                  <Icon name="x" size={14} />
                </button>
              </div>
            </div>
          )}
        </Show>
        <Show when={voice() !== "idle"}>
          <div class="voice-status" role="status" aria-live="polite" data-phase={voice()}>
            <Show when={voiceHost.view().conversation} fallback={<span class="rec-dot" classList={{ quiet: voice() === "sending" }} aria-hidden="true" />}>
              <Icon name="waveform" size={18} class="voice-mark" />
            </Show>
            <span class="voice-label">{voiceHost.view().note || ({ listening: "Listening…", sending: "Sending…", waiting: "Waiting for the answer…", speaking: "Speaking · talk to interrupt", starting: "Starting…", idle: "" })[voice()]}</span>
            <span class="voice-level" aria-hidden="true"><i style={{ transform: `scaleX(${Math.max(0.04, voiceHost.view().level)})` }} /></span>
            <button type="button" class="btn btn-sm btn-ghost" aria-label={voiceHost.view().conversation ? "End voice conversation" : "Cancel dictation"} onClick={() => voiceHost.stop()}>{voiceHost.view().conversation ? "End" : "Cancel"}</button>
          </div>
        </Show>
        <div class="comp-row">
          <Show when={voice() === "idle" && !props.hero}>{attachButton()}</Show>
          <Show when={voiceHost.error()}><PartError error={voiceHost.error()} retry={() => startVoice()} /></Show>
          <div class="comp-field" ref={field}>
            <textarea
              ref={ta}
              rows={1}
              value={text()}
              placeholder={placeholder()}
              aria-label="Message"
              enterkeyhint={coarsePointer() ? "enter" : "send"}
              onInput={onInput}
              onKeyUp={(e) => {
                if (e.key.startsWith("Arrow") || e.key === "Home" || e.key === "End") syncCaret();
              }}
              onClick={syncCaret}
              onKeyDown={onKeyDown}
              onPaste={onPaste}
              onFocus={() => setFocused(true)}
              onBlur={() => setFocused(false)}
            />
          </div>
          <Show when={!props.hero}>
            <div class="comp-actions">
              <Show when={voiceOn() && !voiceHost.view().conversation && (voice() === "idle" || voice() === "starting" || voice() === "listening")}>{micButton()}</Show>
              {actionButtons()}
            </div>
          </Show>
        </div>
        <Show when={props.hero}>
          <div class="comp-bar">
            <Show when={voice() === "idle"}>{attachButton()}</Show>
            <Show when={voiceOn() && !voiceHost.view().conversation && (voice() === "idle" || voice() === "starting" || voice() === "listening")}>{micButton()}</Show>
            <Show when={voice() === "idle" && !touch}>
              <span class="composer-hint">
                <kbd>↵</kbd> send <kbd>⇧↵</kbd> new line <kbd>/</kbd> commands
              </span>
            </Show>
            <span class="spacer" />
            {actionButtons()}
          </div>
        </Show>
        <input
          ref={fileInput}
          type="file"
          accept={priv() ? "image/*,text/*,.md,.json,.csv,.yaml,.yml,.toml,.log,.ts,.tsx,.js,.rs,.py,.go,.sh,.sql" : undefined}
          multiple
          hidden
          onChange={(e) => picked(e.currentTarget)}
        />
        <Show when={touch}>
          {/* The camera app takes one photo; private chats allow it too (it's an image). */}
          <input ref={cameraInput} type="file" accept="image/*" capture="environment" hidden data-camera onChange={(e) => picked(e.currentTarget)} />
        </Show>

        <Show when={mentionMatches().length}>
          <div class="slash-pop mention-pop" role="listbox" aria-label="Mention a bot">
            <For each={mentionMatches()}>
              {(b, i) => (
                <button
                  type="button"
                  role="option"
                  class="slash-item mention-item"
                  classList={{ active: i() === mentionIdx() }}
                  aria-selected={i() === mentionIdx()}
                  onMouseEnter={() => setMentionIdx(i())}
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => completeMention(b)}
                >
                  <BotAvatar bot={b} size={20} />
                  <span class="slash-name" style={{ color: botColor(b.color) }}>
                    {b.name}
                  </span>
                  <span class="slash-args">@{b.handle}</span>
                  <span class="slash-desc">{b.role}</span>
                </button>
              )}
            </For>
          </div>
        </Show>
        <Show when={slashMatches().length}>
          <div class="slash-pop" role="listbox" aria-label="Commands">
            <For each={slashMatches()}>
              {(c, i) => (
                <button
                  type="button"
                  role="option"
                  class="slash-item"
                  classList={{ active: i() === slashIdx() }}
                  aria-selected={i() === slashIdx()}
                  onMouseEnter={() => setSlashIdx(i())}
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => completeSlash(c)}
                >
                  <span class="slash-name">/{c.name}</span>
                  <Show when={c.args}>
                    <span class="slash-args">{c.args}</span>
                  </Show>
                  <span class="slash-desc">{c.desc}</span>
                </button>
              )}
            </For>
          </div>
        </Show>
      </div>
    </div>
  );

  function actionButtons() {
    return (
      <>
        <Show when={busy() && voice() === "idle"}>
          <button type="button" class="send-btn stop" aria-label="Stop" data-tip="Stop the agent" onClick={() => props.session && abortSession(props.session.info.id)}>
            <Icon name="stop" size={14} />
          </button>
        </Show>
        <Show when={(!busy() || hasContent()) && voice() === "idle" && (hasContent() || !voiceOn())}>
          <button
            type="button"
            class="send-btn"
            classList={{ queue: busy() }}
            aria-label={busy() ? "Queue" : "Send"}
            data-tip={sendTip()}
            disabled={!hasContent() || sending()}
            onClick={() => void submit()}
          >
            <Show when={sending()} fallback={<Icon name={busy() ? "cornerDownLeft" : "arrowUp"} size={busy() ? 16 : 18} stroke={2.2} />}>
              <Spinner size={16} />
            </Show>
          </button>
        </Show>
        <Show when={voiceOn() && !!state.server?.voice_engine && !hasContent() && !busy() && voice() === "idle"}>
          <button type="button" class="send-btn voice-call" aria-label="Start voice conversation" data-tip="Talk and hear the answer" disabled={sending()} onClick={() => void startVoice(true)}>
            <Icon name="waveform" size={18} />
          </button>
        </Show>
      </>
    );
  }
}

/** "Pasted text", "Pasted text 2". */
const pastedLabel = (a: Attachment) => a.name.replace(/\.txt$/, "");

/** The preview shows the start of a huge paste (all of it is sent). */
function peekText(t: string): string {
  const MAX = 20_000;
  return t.length > MAX ? `${t.slice(0, MAX)}\n…` : t;
}


// Settings stays the only place to set voice up; exported for the palette.
export const openVoiceSettings = () => openSettings("voice");
