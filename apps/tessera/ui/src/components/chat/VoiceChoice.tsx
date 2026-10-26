import { Show } from "solid-js";
import type { createVoiceHost } from "../../lib/voice-host";

export default function VoiceChoice(props: { host: ReturnType<typeof createVoiceHost> }) {
  return <Show when={props.host.choice()}>{(choice) => <section class="voice-choice" role="region" aria-label="Turn on voice">
    <strong>Voice is off</strong>
    <p>Choose where speech is handled. Nothing is recorded until you choose.</p>
    <Show when={choice().key}>
      <button type="button" class="btn btn-primary" disabled={choice().busy} onClick={() => void props.host.choose("openrouter")}>Use OpenRouter (your key)</button>
      <p class="field-help">Recommended for natural conversation. Paid cloud speech through OpenRouter and its providers.</p>
    </Show>
    <div class="voice-choice-private">
      <button type="button" class="btn" disabled={choice().busy} onClick={() => void props.host.choose("device")}>On this device</button>
      <button type="button" class="btn" disabled={choice().busy} onClick={() => void props.host.choose("server")}>On my server</button>
      <button type="button" class="btn btn-ghost" disabled={choice().busy} onClick={props.host.stop}>Not now</button>
    </div>
    <p class="field-help">Private engines may need an offline voice or a download. Some phones have no offline recognizer.</p>
    <Show when={choice().error}><p class="form-error" role="alert">{choice().error}</p></Show>
  </section>}</Show>;
}
