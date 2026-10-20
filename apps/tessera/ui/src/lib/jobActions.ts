// Scheduled-job actions belong to optional job controls, not first paint.
import { api } from "./store";
import type { Job } from "../proto/Job";
import { confirmDialog, setBotEditor, setJobEditor, setSettingsTab, toast, toastError } from "./ui";
import { openSession } from "./workspace";

export async function runJobNow(j: Job): Promise<boolean> {
  try {
    const info = await api.runJob(j.id);
    toast({ level: "success", message: `“${j.name}” started`, action: { label: "Open", run: () => {
      setSettingsTab(null); setBotEditor(null); setJobEditor(null); openSession(info.id, { newPane: true });
    } } });
    return true;
  } catch (e) { toastError(e, "Run failed"); return false; }
}

export async function deleteJob(j: Job): Promise<boolean> {
  if (!(await confirmDialog({ title: `Delete “${j.name}”?`, body: "Past runs stay in your chat history.", confirmLabel: "Delete", danger: true }))) return false;
  try { await api.deleteJob(j.id); return true; }
  catch (e) { toastError(e); return false; }
}
