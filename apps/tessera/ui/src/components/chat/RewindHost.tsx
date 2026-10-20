// Keep a cancellable host on screen while confirmation code is unavailable.
// This host never imports rewindAction or sends a rewind (including dry runs).
import { Show } from "solid-js";
import { onDemand } from "../../lib/onDemand";
import { setRewindDialog, type RewindSpec } from "../../lib/ui";
import { PartError } from "../PartError";
import { Modal } from "../primitives";

const rewind = onDemand(() => import("./Rewind"), "Rewind");

export function RewindHost(props: { spec: RewindSpec }) {
  if (!rewind.error()) void rewind.load();
  const close = () => setRewindDialog(null);
  return <Show when={rewind.get()} fallback={
    <Modal title="Rewind to here" size="md" class="rewind-dialog rewind-chunk" onClose={close}>
      <div class="modal-body">
        <Show when={rewind.error()} fallback={<p role="status" aria-busy="true">Loading rewind preview…</p>}>
          <PartError error={rewind.error()} retry={rewind.retry} />
        </Show>
      </div>
      <footer class="rewind-foot"><button type="button" class="btn btn-ghost" onClick={close}>Cancel</button></footer>
    </Modal>
  }>
    {(m) => {
      const Dialog = m().RewindDialog;
      return <Dialog spec={props.spec} />;
    }}
  </Show>;
}
