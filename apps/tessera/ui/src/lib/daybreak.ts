// Account-specific controls are needed only inside chat options.
import { paneSessionId, paneSettings, patchPane } from "./actions";
import { api, findModel, state, upsertSession } from "./store";
import type { ModelInfo } from "../proto/ModelInfo";
import { toastError } from "./ui";
import { updateChatDraft } from "./workspace";

const blue = (m?: ModelInfo) => !!m?.cyber_access_programs?.includes("daybreak_blue");
/** OpenAI's deprecated alias: never a model to move a chat to. */
const ALIAS = "gpt-daybreak-blue-latest";

/** Daybreak Blue for this chat: on its own model, or by moving it to `target`. */
export type DaybreakOffer = { here: true } | { here: false; target: ModelInfo };

/** The Blue model closest to `model`: the same family (the part after the
 * last dash, like "sol"), newest version first; else the newest one. */
export function closestBlue(model: Pick<ModelInfo, "id">, models: ModelInfo[]): ModelInfo | undefined {
  const usable = models.filter((m) => blue(m) && m.id !== ALIAS);
  const family = (id: string) => id.split("-").pop() ?? "";
  const version = (id: string) => parseFloat(/\d+(?:\.\d+)?/.exec(id)?.[0] ?? "0");
  const same = usable.filter((m) => family(m.id) === family(model.id));
  return [...(same.length ? same : usable)].sort((a, b) => version(b.id) - version(a.id))[0];
}

/** Like Codex, the account's catalog decides: it offers Blue on this model
 * (or lists no programs for it while it offers Blue on another of its
 * models). When it lists programs for this model without Blue, the closest
 * model it does offer Blue on is the way to it. Null: not from this account. */
export function daybreakOffer(paneId: string): DaybreakOffer | null {
  const model = findModel(paneSettings(paneId).model);
  const provider = state.providers.find((p) => p.id === model?.provider);
  if (!model || !provider?.authenticated || !["openai_codex", "openai_responses"].includes(provider.kind)) return null;
  if (!provider.models.some(blue)) return null;
  if (!model.cyber_access_programs?.length || blue(model)) return { here: true };
  const target = closestBlue(model, provider.models);
  return target ? { here: false, target } : null;
}

export function daybreakAvailable(paneId: string) {
  return daybreakOffer(paneId)?.here === true;
}

export async function setPaneDaybreak(paneId: string, on: boolean) {
  const offer = daybreakOffer(paneId);
  if (on && !offer) return;
  // Turning it on where this model lacks it: the chat moves to the model
  // the option named (the user saw that before tapping).
  if (on && offer && !offer.here) await patchPane(paneId, { model: { provider: offer.target.provider, model: offer.target.id } });
  if (on && !daybreakAvailable(paneId)) return;
  const sid = paneSessionId(paneId);
  if (!sid) { updateChatDraft(paneId, { daybreak: on }); return; }
  try { upsertSession(await api.setSessionDaybreak(sid, on)); }
  catch (e) { toastError(e, "Could not update Daybreak Blue"); }
}
