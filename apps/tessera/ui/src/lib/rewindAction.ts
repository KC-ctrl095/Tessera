// Confirmation belongs to the already-lazy rewind dialog, not the initial chat bundle.
import { unwrap } from "solid-js/store";
import type { ContentPart } from "../proto/ContentPart";
import type { RewindResult } from "../proto/RewindResult";
import { messageAttachments, typedText } from "./images";
import { REWIND_FADE_MS, api, applyRewind, beginLocalRewind, cancelLocalRewind, state, upsertSession } from "./store";
import { setRewindDialog, toast, type RewindSpec } from "./ui";
import { plural, reducedMotion, truncate } from "./util";
import { focusComposer, getDraft, openSession, paneSession, paneShowing, setDraft, stageAttachments } from "./workspace";
import { historyTo } from "./history";
export const fileChanges = (r: RewindResult) => r.restored.length + r.deleted.length;

const SUFFIX = " (rewound)";
/** The chat that keeps the full history ("<title> (rewound)", like the server names it). */
export const branchTitle = (title: string) => `${title}${SUFFIX}`;
/** Short enough for a line of text, keeping the "(rewound)". */
export const shortTitle = (title: string) =>
  title.endsWith(SUFFIX) ? truncate(title.slice(0, -SUFFIX.length), 48) + SUFFIX : truncate(title, 58);

/**
 * Rewind for real, as chosen in the dialog. `fadeFrom` is where the preview
 * said the cut goes: the rows after it fade out while the server works.
 * Closes the dialog on success; throws (with everything left as it was) on failure.
 */
export async function confirmRewind(spec: RewindSpec, o: { files: boolean; conversation: boolean; fadeFrom: number }): Promise<void> {
  const { sessionId, seq } = spec;
  await historyTo(sessionId, seq);
  const message = state.sessions[sessionId]?.entries.find((e) => e.seq === seq && e.kind === "user");
  // Kept before the entry leaves the store (its parts are never changed in place).
  const content: ContentPart[] = message?.kind === "user" ? unwrap(message.content) : [];
  const started = performance.now();
  if (o.conversation) beginLocalRewind(sessionId, o.fadeFrom);
  let res: RewindResult;
  try {
    res = await api.rewind(sessionId, Math.max(0, seq - 1), { files: o.files, conversation: o.conversation });
  } catch (e) {
    cancelLocalRewind(sessionId);
    throw e;
  }
  // Let the rows finish fading before they go.
  const left = REWIND_FADE_MS - (performance.now() - started);
  if (o.conversation && left > 0 && !reducedMotion()) await new Promise((r) => setTimeout(r, left));
  upsertSession(res.session);
  if (res.branch) upsertSession(res.branch);
  if (o.conversation) applyRewind(sessionId, res.to_seq);
  setRewindDialog(null);
  const missing = o.conversation ? restoreMessage(spec, content) : [];
  announce(res, o, missing);
}

/** Put the rewound message back in the composer (before anything already typed there). */
function restoreMessage(spec: RewindSpec, content: ContentPart[]): string[] {
  const pane = spec.paneId && paneSession(spec.paneId)?.info.id === spec.sessionId ? spec.paneId : paneShowing(spec.sessionId);
  if (!pane) return [];
  const text = typedText(content).replace(/\s+$/, "");
  const draft = getDraft(pane);
  setDraft(pane, draft.trim() && draft.trim() !== text.trim() ? `${text}\n\n${draft}` : text);
  const { attachments, missing } = messageAttachments(content);
  stageAttachments(pane, attachments);
  focusComposer(pane);
  return missing;
}

function announce(res: RewindResult, o: { files: boolean; conversation: boolean }, missing: string[]) {
  const parts = [o.conversation ? "Rewound" : "Files rewound"];
  if (o.files) {
    if (res.restored.length) parts.push(`${plural(res.restored.length, "file", "files")} restored`);
    if (res.deleted.length) parts.push(`${plural(res.deleted.length, "new file", "new files")} removed`);
  }
  const branch = res.branch;
  if (branch) parts.push(`kept the full chat as “${shortTitle(branch.title)}”`);
  toast({
    level: "success",
    message: parts.join(" · "),
    timeout: branch ? 9000 : 5000,
    action: branch ? { label: "Open", run: () => openSession(branch.id, { newPane: true }) } : undefined,
  });
  if (missing.length) {
    toast({
      level: "info",
      message: `${missing.join(", ")} ${missing.length === 1 ? "is" : "are"} still on the server but not back in the message box: attach ${missing.length === 1 ? "it" : "them"} again if needed.`,
      timeout: 7000,
    });
  }
}
