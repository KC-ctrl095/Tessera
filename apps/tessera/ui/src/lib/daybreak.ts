// Account-specific controls are needed only inside chat options.
import { paneSessionId, paneSettings } from "./actions";
import { api, findModel, state, upsertSession } from "./store";
import type { ModelInfo } from "../proto/ModelInfo";
import { toastError } from "./ui";
import { updateChatDraft } from "./workspace";

const blue = (m?: ModelInfo) => !!m?.cyber_access_programs?.includes("daybreak_blue");

/** Like Codex: the account's catalog offers Blue on this model, or on any of
 * its models when it lists no programs for this one. */
export function daybreakAvailable(paneId: string) {
  const model = findModel(paneSettings(paneId).model);
  const provider = state.providers.find((p) => p.id === model?.provider);
  if (!model || !provider?.authenticated || !["openai_codex", "openai_responses"].includes(provider.kind)) return false;
  return model.cyber_access_programs?.length ? blue(model) : provider.models.some(blue);
}

export async function setPaneDaybreak(paneId: string, on: boolean) {
  if (on && !daybreakAvailable(paneId)) return;
  const sid = paneSessionId(paneId);
  if (!sid) { updateChatDraft(paneId, { daybreak: on }); return; }
  try { upsertSession(await api.setSessionDaybreak(sid, on)); }
  catch (e) { toastError(e, "Could not update Daybreak Blue"); }
}
