// Settings → Web & voice: private speech first, cloud services only by choice.
import { For, Show, createResource, createSignal, onCleanup } from "solid-js";
import type { VoiceConfig } from "../../proto/VoiceConfig";
import { prefs, setPrefs } from "../../lib/prefs";
import { api, setState, state } from "../../lib/store";
import { errorMessage, toast } from "../../lib/ui";
import { Icon, Spinner } from "../icons";
import { Toggle } from "../primitives";
import { Advanced, Card, LoadingBlock, Row, ThisDevice } from "./kit";

const choices = [
  ["auto", "Automatic", "Private speech on this device, otherwise your server. No cloud speech."],
  ["device", "On this device", "Offline engines only. Support depends on the device and language."],
  ["server", "On my server", "Whisper and Piper on your own server. Downloads once on first use."],
  ["openai", "OpenAI with my key", "Paid speech services. Your audio goes to OpenAI only when you choose this."],
  ["off", "Off", "Hide the microphone and voice conversation buttons."],
];
const services = [
  ["local", "Whisper on another machine", "http://127.0.0.1:8080/v1", "whisper-large-v3-turbo"],
  ["groq", "Groq (cloud)", "https://api.groq.com/openai/v1", "whisper-large-v3-turbo"],
  ["openai", "OpenAI (cloud)", "https://api.openai.com/v1", "gpt-4o-mini-transcribe"],
  ["custom", "Another service", "", ""],
];
export function VoiceTab() {
  const [cfg, { mutate, refetch }] = createResource(() => api.getVoice());
  const timer = setInterval(() => { if (cfg()?.server?.state === "installing") void refetch(); }, 1000);
  onCleanup(() => clearInterval(timer));
  return <Show when={cfg()} fallback={<LoadingBlock error={cfg.error} />}><VoiceForm initial={cfg()!} status={() => cfg()!} onSaved={mutate} /></Show>;
}
function VoiceForm(props: { initial: VoiceConfig; status: () => VoiceConfig; onSaved: (c: VoiceConfig) => void }) {
  const init = props.initial;
  const modern = !!init.engine;
  const [engine, setEngine] = createSignal(init.engine || (init.enabled ? "service" : "off"));
  const [provider, setProvider] = createSignal(init.provider || "local");
  const [base, setBase] = createSignal(init.base_url);
  const [model, setModel] = createSignal(init.model);
  const [language, setLanguage] = createSignal(init.language);
  const [serverModel, setServerModel] = createSignal(init.server_model || "auto");
  const [serverVoice, setServerVoice] = createSignal(init.server_voice || "");
  const [keep, setKeep] = createSignal(!!init.keep_audio);
  const [key, setKey] = createSignal("");
  const [hasKey, setHasKey] = createSignal(init.has_key);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const server = () => props.status().server;
  const form = () => JSON.stringify([engine(), provider(), base().trim(), model().trim(), language().trim(), serverModel(), serverVoice(), keep()]);
  const [kept, setKept] = createSignal(form());
  const dirty = () => form() !== kept() || key().trim() !== "";
  const save = async (e?: Event) => {
    e?.preventDefault(); setError("");
    if (engine() === "openai" && !props.status().openai_key) { setError("Save your OpenAI API key in Passwords & keys as OPENAI_API_KEY, or add OpenAI under Models. A ChatGPT sign-in does not pay for speech."); return; }
    if (engine() === "service" && (!/^https?:\/\//.test(base()) || !model().trim())) { setError("Enter the service's address and speech model."); return; }
    if (engine() === "service" && ["groq", "openai"].includes(provider()) && !hasKey() && !key().trim()) { setError("Paste the API key for the transcription service."); return; }
    const submitted = form(), submittedKey = key();
    setBusy(true);
    try {
      const config = { ...props.status(), engine: engine(), enabled: engine() === "service", provider: provider(), base_url: base().trim(), model: model().trim(), language: language().trim(), server_model: serverModel(), server_voice: serverVoice(), keep_audio: keep() };
      const saved = await api.saveVoice(config, submittedKey.trim() || null);
      props.onSaved(saved); setHasKey(saved.has_key); if (key() === submittedKey) setKey(""); setKept(submitted);
      try { setState("server", await api.hello()); } catch { if (state.server) { setState("server", "voice_engine", saved.engine); setState("server", "voice", saved.enabled); } }
      toast({ level: "success", message: saved.engine === "off" ? "Voice turned off" : "Voice settings saved", timeout: 2200 });
      return saved;
    } catch (e) { setError(errorMessage(e)); } finally { setBusy(false); }
  };
  const install = async () => {
    if (!await save()) return;
    try { props.onSaved(await api.installVoice()); } catch (e) { setError(errorMessage(e)); }
  };
  return (
    <form class="stack" onSubmit={save}>
      <Card>
        <div class="setting-title">Voice</div>
        <p class="field-help">Talk to your chat and hear it answer. Automatic never uses cloud speech. The text still goes to the chat model you selected.</p>
        <Show when={!modern}><p class="field-help warn-note">Update your Tessera server for private speech and spoken answers. This server supports transcription services only.</p></Show>
        <label class="field"><span class="field-label">Where speech is handled</span>
          <select class="input" aria-label="Where speech is handled" value={engine()} onChange={(e) => setEngine(e.currentTarget.value)}>
            <For each={choices.filter(([id]) => modern || id === "off")} >{([id, label]) => <option value={id}>{label}</option>}</For>
            <option value="service">Another transcription service</option>
          </select>
        </label>
        <p class="field-help">{choices.find(([id]) => id === engine())?.[2] || "Audio goes from your server to the service below. Spoken answers use an offline device voice or Piper on your server."}</p>
        <Show when={engine() === "openai"}>
          <p class="field-help" classList={{ "warn-note": !props.status().openai_key }}><Icon name={props.status().openai_key ? "check" : "alert"} size={13} /> {props.status().openai_key ? "OpenAI API key saved. Uses paid transcription and speech, not your ChatGPT plan." : "Save an OpenAI API key in Privacy & safety → Passwords & keys (OPENAI_API_KEY), or under Models."}</p>
          <p class="field-help">This uses your chosen chat model between transcription and speech. OpenAI Realtime is not available yet.</p>
        </Show>
      </Card>
      <Show when={engine() !== "off"}>
        <Card>
          <div class="setting-title">Talking to a chat</div>
          <p class="field-help">Tap the microphone to dictate, then check the words before sending. Hold it while you speak and let go to send at once.</p>
          <p class="field-help">When the message box is empty, the waves button starts a conversation. Pause to send; hear the answer, and start talking to interrupt it. Headphones help avoid echoes.</p>
          <p class="field-help">Tap End or press Escape to turn the microphone and speech off. Work already sent to the chat continues unless you use its Stop button.</p>
        </Card>
        <Card>
          <Advanced label="More">
            <label class="field"><span class="field-label">Language <span class="opt">optional</span></span><input class="input" aria-label="Speech language" value={language()} onInput={(e) => setLanguage(e.currentTarget.value)} placeholder="This device's language, or en, nl, de…" maxLength={16} /></label>
            <Show when={modern}>
              <label class="field"><span class="field-label">Server's speech model</span><select class="input" value={serverModel()} onChange={(e) => setServerModel(e.currentTarget.value)}><option value="auto">Automatic for this computer</option><option value="tiny">Tiny · faster on older computers</option><option value="base">Base · more accurate</option></select></label>
              <label class="field"><span class="field-label">Server's voice</span><select class="input" value={serverVoice()} onChange={(e) => setServerVoice(e.currentTarget.value)}><option value="">Choose by language</option><For each={server()?.voices}>{(v) => <option value={v.id}>{v.label}{v.installed ? " · downloaded" : ""}</option>}</For></select></label>
              <Row title="Keep recordings" help="Off by default. When on, audio sent to your server is saved in its voice/recordings folder. Device-only speech is never saved here."><Toggle checked={keep()} onChange={setKeep} label="Keep recordings" /></Row>
              <Show when={(props.status().kept ?? 0) > 0}><button type="button" class="btn btn-sm btn-ghost" onClick={() => { void api.deleteVoiceRecordings().then(props.onSaved).catch((e) => setError(errorMessage(e))); }}>Delete {props.status().kept} saved recordings</button></Show>
            </Show>
            <Row title="Send right after dictating" help={<><ThisDevice /> Off: tap to dictate and check the text first. Holding the mic or using voice conversation sends automatically.</>}><Toggle checked={prefs.voiceAutoSend} onChange={(v) => setPrefs("voiceAutoSend", v)} label="Send automatically after transcription" /></Row>
          </Advanced>
        </Card>
      </Show>
      <Show when={engine() === "service"}>
        <Card>
          <div class="setting-title">Another transcription service</div>
          <label class="field"><span class="field-label">Service</span><select class="input" aria-label="Transcription service" value={provider()} onChange={(e) => { const p = services.find(([id]) => id === e.currentTarget.value)!; setProvider(p[0]); setBase(p[2]); setModel(p[3]); }}><For each={services}>{([id, label]) => <option value={id}>{label}</option>}</For></select></label>
          <label class="field"><span class="field-label">Address</span><input class="input mono" value={base()} onInput={(e) => setBase(e.currentTarget.value)} placeholder="http://127.0.0.1:8080/v1" /></label>
          <label class="field"><span class="field-label">Model</span><input class="input mono" value={model()} onInput={(e) => setModel(e.currentTarget.value)} /></label>
          <label class="field"><span class="field-label">API key <span class="opt">if required</span></span><input class="input mono" type="password" value={key()} onInput={(e) => setKey(e.currentTarget.value)} placeholder={hasKey() ? "Key saved (never shown again)" : "Paste your service key"} autocomplete="off" /></label>
          <p class="field-help">Any OpenAI-compatible /audio/transcriptions endpoint. Keys stay on your server. Cloud providers may charge you.</p>
        </Card>
      </Show>
      <Show when={modern && ["auto", "server", "service"].includes(engine())}>
        <Card>
          <Row title="Server's private engines" help={server()?.state === "ready" ? "Whisper and Piper are ready." : "Downloaded from official releases, with fixed SHA-256 fingerprints. No account or API key."}>
            <button type="button" class="btn btn-sm" disabled={busy() || server()?.state === "installing"} onClick={() => void install()}>{server()?.state === "installing" ? "Downloading…" : server()?.state === "ready" ? "Check downloads" : "Download engines"}</button>
          </Row>
          <Show when={server()?.state === "installing"}><progress class="voice-progress" max={server()?.total_bytes || 1} value={server()?.done_bytes || 0} /><p class="field-help">{Math.round((server()?.done_bytes || 0) / 1048576)} MB of {Math.round((server()?.total_bytes || server()?.download_bytes || 0) / 1048576)} MB</p></Show>
          <Show when={server()?.error}><p class="form-error">{server()?.error}</p></Show>
        </Card>
      </Show>
      <Show when={error()}><div class="form-error">{error()}</div></Show>
      <Show when={dirty() || busy()}><div class="form-actions sticky-actions"><button type="submit" class="btn btn-primary" disabled={busy()}><Show when={busy()}><Spinner size={14} /></Show>Save voice settings</button></div></Show>
    </form>
  );
}
