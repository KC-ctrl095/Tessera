// When to offer first-run setup: after connecting to a server that hasn't
// been set up (once per server per run). Kept apart from the wizard itself
// (components/Setup.tsx), which loads only when it opens.
import { activeServer } from "./app";
import { api, onBootstrapped } from "./store";
import { setSetupOpen } from "./ui";

const checked = new Set<string>();

export function watchSetup() {
  onBootstrapped(() => {
    const id = activeServer()?.id ?? "web";
    if (checked.has(id)) return;
    checked.add(id);
    api
      .setupStatus()
      .then((s) => {
        if (!s.completed) setSetupOpen(true);
      })
      .catch(() => {
        /* older server without setup_status */
      });
  });
}
