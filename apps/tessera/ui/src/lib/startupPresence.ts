// Native cold-start entries are retained until these watchers are installed,
// just after first paint, before a paired connection normally becomes ready.
import { watchNotifications } from "./notificationPresence";
import { isAndroidApp } from "./native";
import { toastError } from "./ui";

export function watchPresence() {
  watchNotifications();
  if (isAndroidApp) void import("./assistant-native").then((m) => m.watchNativeAssistant()).catch((e) => toastError(e, "Phone assistant"));
}
