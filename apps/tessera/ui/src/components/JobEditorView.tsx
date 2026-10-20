// A scheduled task's editor, a view of its own (a full screen on phones):
// New → More → Scheduled task makes one; a task in the sidebar's Scheduled
// tab opens here. A bot's tasks open here too, kept as that bot's.
import type { Job } from "../proto/Job";
import { setJobEditor } from "../lib/ui";
import { JobEditor, emptyJob } from "./Jobs";
import { Modal } from "./primitives";

export function JobEditorView(props: { job: Job | null }) {
  const close = () => setJobEditor(null);
  const job = props.job ? { ...props.job } : emptyJob();
  return (
    <Modal onClose={close} title={props.job ? "Edit scheduled task" : "New scheduled task"} size="lg" class="job-editor-view" hideHeader>
      <div class="modal-body">
        <JobEditor job={job} onDone={close} lockBot={!!props.job?.bot_id} />
      </div>
    </Modal>
  );
}
