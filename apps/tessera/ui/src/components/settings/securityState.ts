// The server's security settings (`get_security` / `set_security`), one copy
// for Privacy & safety and its page "What commands can reach". Everything saves
// at once; choices that lower protection ask first (in the sections). Only the
// user changes these: agents have no request for them.
import { createSignal } from "solid-js";
import type { SecurityInfo } from "../../proto/SecurityInfo";
import { call } from "../../lib/store";
import { RpcError } from "../../lib/transport";
import { toastError } from "../../lib/ui";

export type SecurityPatch = Partial<
  Pick<SecurityInfo, "sandbox" | "folders" | "ask_outside" | "redact_patterns" | "save_secrets_from_chat" | "secret_saving" | "secret_machines" | "secret_machines_picked" | "bot_handoff">
>;

const [info, setInfo] = createSignal<SecurityInfo | null>(null);
const [error, setError] = createSignal<unknown>(null);
/** What is being saved right now ("sandbox", "folders", a switch's key). */
const [busy, setBusy] = createSignal<string | null>(null);
export { busy as securityBusy, error as securityError, info as security };

/** Read them again (each time a section shows them: another device may have changed them). */
export async function loadSecurity() {
  try {
    setInfo(await call<SecurityInfo>({ method: "get_security" }));
    setError(null);
  } catch (e) {
    setError(e);
  }
}

export async function saveSecurity(what: string, patch: SecurityPatch): Promise<boolean> {
  setBusy(what);
  try {
    setInfo(await call<SecurityInfo>({ method: "set_security", params: patch }));
    return true;
  } catch (e) {
    toastError(e, "Couldn't change the security settings");
    return false;
  } finally {
    setBusy(null);
  }
}

/** A server from before these settings. */
export const oldSecurityServer = () => {
  const e = error();
  return e instanceof RpcError && (e.code === "bad_request" || e.code === "not_found") && /unknown|variant|method/i.test(e.message);
};
