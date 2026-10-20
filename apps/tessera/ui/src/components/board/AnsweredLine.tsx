// An answered question on a whiteboard: one quiet line (in the chat's card and on the tile).
// Apart from AskPanel so the chat's first download doesn't need the question itself.
import { Icon } from "../icons";

/** An ask that was answered from this device: one quiet line. */
export function AnsweredLine(props: { label: string; prompt: string; signed?: boolean }) {
  return (
    <div class="ask-done" role="status">
      <span class="ask-done-mark" aria-hidden="true">
        <Icon name="check" size={12} stroke={2.4} />
      </span>
      <span class="ask-done-label">{props.label}</span>
      <span class="ask-done-prompt">{props.prompt}</span>
    </div>
  );
}
