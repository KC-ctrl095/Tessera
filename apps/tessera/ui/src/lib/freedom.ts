// How much freedom new chats get: one choice for all the user's devices, held
// by the server (`SecurityInfo.default_permission`). On a server from before
// that, or where nobody has chosen yet, each device keeps its own
// (`prefs.defaultPermission`), as before. A device whose own earlier choice
// differs from the shared one asks once which to keep.
import { createSignal } from "solid-js";
import type { PermissionMode } from "../proto/PermissionMode";
import type { SecurityInfo } from "../proto/SecurityInfo";
import { permissionInfo } from "./actions";
import { prefs, setPrefs } from "./prefs";
import { call, onBootstrapped } from "./store";
import { confirmDialog, toastError } from "./ui";

const [shared, setShared] = createSignal<PermissionMode | null>(null);

/** How much freedom a new chat gets on this device now. */
export const newChatFreedom = (): PermissionMode => shared() ?? prefs.defaultPermission;

/** Change it for every device (this one keeps it too, for a server from before). */
export async function setNewChatFreedom(mode: PermissionMode): Promise<boolean> {
  setPrefs("defaultPermission", mode);
  try {
    const info = await call<SecurityInfo>({ method: "set_security", params: { default_permission: mode } });
    setShared(info.default_permission ?? null);
    return true;
  } catch (e) {
    toastError(e, "Couldn't change how much freedom new chats get");
    return false;
  }
}

/** Read the shared choice (on every connection: another device may have changed it). */
export async function loadNewChatFreedom() {
  let info: SecurityInfo;
  try {
    info = await call<SecurityInfo>({ method: "get_security" });
  } catch {
    return;
  }
  const theirs = info.default_permission ?? null;
  setShared(theirs);
  const mine = prefs.defaultPermission;
  if (!theirs || mine === theirs) return;
  // This device never chose: it follows the others without a question.
  if (mine === "default") return void setPrefs("defaultPermission", theirs);
  const [m, t] = [permissionInfo(mine).label, permissionInfo(theirs).label];
  const keepMine = await confirmDialog({
    title: "One choice for all your devices",
    body: `New chats on your other devices start with “${t}”; this device used “${m}”. It's one setting for all of them now (Settings → Privacy & safety).`,
    confirmLabel: `Use “${m}”`,
    cancelLabel: `Keep “${t}”`,
    danger: mine === "auto",
  });
  if (keepMine) await setNewChatFreedom(mine);
  else setPrefs("defaultPermission", theirs);
}

onBootstrapped(() => void loadNewChatFreedom());
