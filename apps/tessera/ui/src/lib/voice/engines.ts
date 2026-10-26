import { isTauri } from "../util";
import { checkAbort, cancellable, startRecording, type Recorder } from "../voice";

interface RecognitionResult { isFinal: boolean; [index: number]: { transcript: string } }
interface Recognition {
  processLocally: boolean; lang: string; interimResults: boolean; continuous: boolean;
  onresult: ((e: { results: ArrayLike<RecognitionResult> }) => void) | null;
  onerror: ((e: { error: string }) => void) | null; onend: (() => void) | null;
  start(): void; stop(): void; abort(): void;
}
interface RecognitionClass {
  new(): Recognition;
  available?: (options: { langs: string[]; processLocally: boolean }) => Promise<string>;
}
export interface Input { finish(): void; cancel(): void; recorder?: Recorder }
const android = () => isTauri && /Android/i.test(navigator.userAgent);
const recognizer = () => (window as unknown as { SpeechRecognition?: RecognitionClass; webkitSpeechRecognition?: RecognitionClass }).SpeechRecognition ?? (window as unknown as { webkitSpeechRecognition?: RecognitionClass }).webkitSpeechRecognition;
async function native<T>(command: string, args: Record<string, unknown> = {}, signal?: AbortSignal): Promise<T> {
  checkAbort(signal);
  const { invoke } = await import("@tauri-apps/api/core");
  checkAbort(signal); // no await between admission and invoke
  const result = await cancellable(invoke<T>(command, args), signal);
  checkAbort(signal);
  return result;
}
export async function support(language: string, signal?: AbortSignal): Promise<{ listen: boolean; speak: boolean }> {
  checkAbort(signal);
  if (android()) {
    let s = await native<{ listen: boolean; speak: boolean; initializing?: boolean }>("voice_status", { language }, signal);
    for (let i = 0; i < 5 && s.initializing; i++) {
      await new Promise((r) => setTimeout(r, 300));
      s = await native("voice_status", { language }, signal);
    }
    return s;
  }
  const R = recognizer();
  let listen = false;
  // Older Web Speech APIs use their maker's cloud. Never start those.
  if (R?.available) {
    try { listen = await R.available({ langs: [language], processLocally: true }) === "available"; } catch { /* server fallback */ }
  }
  if (typeof speechSynthesis !== "undefined" && !speechSynthesis.getVoices().length) {
    await new Promise<void>((resolve) => { const timer = setTimeout(done, 500); function done() { clearTimeout(timer); speechSynthesis.removeEventListener("voiceschanged", done); resolve(); } speechSynthesis.addEventListener("voiceschanged", done); });
  }
  checkAbort(signal);
  return { listen, speak: typeof speechSynthesis !== "undefined" && speechSynthesis.getVoices().some((v) => v.localService && v.lang.split("-")[0] === language.split("-")[0]) };
}

let listenGeneration = 0;
export async function listen(language: string, device: boolean, words: (text: string, final: boolean) => void, failed: (e: Error) => void, signal?: AbortSignal): Promise<Input> {
  checkAbort(signal);
  if (!device) { const recorder = await startRecording(signal); checkAbort(signal); return { recorder, finish() {}, cancel: () => recorder.cancel() }; }
  if (android()) {
    const gen = ++listenGeneration, operation = crypto.randomUUID();
    const { Channel, invoke } = await import("@tauri-apps/api/core");
    checkAbort(signal);
    if (gen !== listenGeneration) throw new DOMException("Voice replaced", "AbortError");
    const events = new Channel<{ text?: string; final?: boolean; error?: string }>();
    events.onmessage = (e) => { if (e.error) failed(new Error(e.error)); else if (e.text !== undefined) words(e.text, !!e.final); };
    const cancel = () => { events.onmessage = () => {}; if (gen === listenGeneration) { listenGeneration++; void invoke("voice_cancel", { operation }).catch(() => {}); } };
    signal?.addEventListener("abort", cancel, { once: true });
    try { checkAbort(signal); await cancellable(invoke("voice_listen", { language, events, operation }), signal); checkAbort(signal); }
    catch (e) { signal?.removeEventListener("abort", cancel); cancel(); throw e; }
    return { finish: () => { if (!signal?.aborted && gen === listenGeneration) void invoke("voice_finish", { operation }).catch(failed); }, cancel: () => { signal?.removeEventListener("abort", cancel); cancel(); } };
  }
  const R = recognizer();
  if (!R?.available) throw new Error("Private speech recognition isn't available on this device. Choose Automatic or On my server.");
  const r = new R();
  r.processLocally = true; r.lang = language; r.interimResults = true; r.continuous = false;
  r.onresult = (e) => {
    let text = "", final = true;
    for (let i = 0; i < e.results.length; i++) { text += e.results[i][0].transcript; final &&= e.results[i].isFinal; }
    words(text.trim(), final);
  };
  let active = true;
  r.onerror = (e) => { if (e.error !== "no-speech" && e.error !== "aborted") { active = false; failed(new Error(`Speech recognition stopped (${e.error}). Try On my server.`)); } };
  r.onend = () => { if (active) { try { r.start(); } catch { active = false; failed(new Error("Could not restart the microphone.")); } } };
  checkAbort(signal);
  const cancel = () => { active = false; r.onresult = null; r.onerror = null; r.onend = null; r.abort(); };
  signal?.addEventListener("abort", cancel, { once: true });
  r.start();
  return { finish() { active = false; r.stop(); }, cancel() { signal?.removeEventListener("abort", cancel); cancel(); } };
}

export interface Playback { play(text: string, audio?: { audio: string; mime: string }): Promise<void>; cancel(): void }
export function playback(language: string, device: boolean): Playback {
  let cancel: (() => void) | undefined;
  return {
    async play(text, audio) {
      if (device && android()) {
        const abort = new AbortController(), operation = crypto.randomUUID();
        cancel = () => abort.abort();
        const { invoke } = await import("@tauri-apps/api/core");
        checkAbort(abort.signal);
        cancel = () => { abort.abort(); void invoke("voice_quiet", { operation }).catch(() => {}); };
        await cancellable(invoke("voice_say", { language, text, operation }), abort.signal);
        checkAbort(abort.signal);
        return;
      }
      return new Promise<void>((resolve, reject) => {
        let ended = false;
        const finish = (error?: Error) => { if (ended) return; ended = true; cancel = undefined; error ? reject(error) : resolve(); };
        if (device) {
          const voice = speechSynthesis.getVoices().find((v) => v.localService && v.lang.split("-")[0] === language.split("-")[0]);
          if (!voice) return finish(new Error("No offline voice for this language is installed. Choose Automatic or On my server."));
          const utterance = new SpeechSynthesisUtterance(text); utterance.voice = voice; utterance.lang = language;
          utterance.onend = () => finish(); utterance.onerror = () => finish(new Error("Could not speak on this device."));
          cancel = () => { utterance.onend = utterance.onerror = null; speechSynthesis.cancel(); finish(); };
          speechSynthesis.speak(utterance);
        } else {
          if (!audio) return finish(new Error("No audio came from the server."));
          // Only audio, and no more than the server's own 16 MiB limit (base64: 4/3).
          if (!/^audio\/[a-z0-9.+-]+$/i.test(audio.mime) || audio.audio.length > 22_400_000) return finish(new Error("The server sent something that isn't speech."));
          const bytes = Uint8Array.from(atob(audio.audio), (c) => c.charCodeAt(0));
          const url = URL.createObjectURL(new Blob([bytes], { type: audio.mime }));
          const player = new Audio(url);
          const cleanup = () => { player.pause(); player.removeAttribute("src"); player.load(); URL.revokeObjectURL(url); };
          player.onended = () => { cleanup(); finish(); };
          player.onerror = () => { cleanup(); finish(new Error("Could not play speech. Tap voice again to retry.")); };
          cancel = () => { player.onended = player.onerror = null; cleanup(); finish(); };
          void player.play().catch(() => { cleanup(); finish(new Error("Audio playback was blocked. Tap voice again to retry.")); });
        }
      });
    },
    cancel() { cancel?.(); cancel = undefined; },
  };
}
