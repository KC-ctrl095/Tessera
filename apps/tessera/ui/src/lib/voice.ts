// Loaded only when the user starts voice. WAV works with whisper.cpp without ffmpeg.
export interface Recording { audio: string; mime: string; ms: number }
export interface Recorder {
  readonly startedAt: number;
  level(): number;
  snapshot(): Recording;
  reset(prerollMs?: number): void;
  stop(): Promise<Recording>;
  cancel(): void;
}

export class VoiceError extends Error {
  constructor(public kind: "denied" | "missing" | "insecure" | "unsupported" | "failed", message: string) { super(message); }
}
export function voiceError(error: unknown): string {
  const name = (error as { name?: string })?.name;
  if (name === "NotAllowedError" || name === "PermissionDeniedError") return "Microphone permission was denied. Allow the microphone in your browser or system settings.";
  if (name === "NotFoundError" || name === "DevicesNotFoundError") return "No microphone was found. Connect one and try again.";
  if (name === "NotReadableError" || name === "TrackStartError") return "The microphone may be in use by another app. Close it and try again.";
  if (name === "SecurityError") return "Microphone access needs HTTPS or the Tessera app.";
  if (name === "NotSupportedError") return "This browser can't capture speech. Try the Tessera app or another browser.";
  return error instanceof Error && error.message ? error.message : "Could not capture audio on this device.";
}

export function voiceSupported(): boolean {
  return !!navigator.mediaDevices?.getUserMedia && !!(window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext);
}

export function wav(samples: Float32Array, rate: number): string {
  const count = Math.floor(samples.length * 16000 / rate);
  const bytes = new Uint8Array(44 + count * 2);
  const view = new DataView(bytes.buffer);
  const word = (at: number, text: string) => { for (let i = 0; i < text.length; i++) bytes[at + i] = text.charCodeAt(i); };
  word(0, "RIFF"); view.setUint32(4, bytes.length - 8, true); word(8, "WAVEfmt ");
  view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true);
  view.setUint32(24, 16000, true); view.setUint32(28, 32000, true); view.setUint16(32, 2, true); view.setUint16(34, 16, true);
  word(36, "data"); view.setUint32(40, count * 2, true);
  for (let i = 0; i < count; i++) {
    const from = Math.floor(i * rate / 16000), to = Math.max(from + 1, Math.floor((i + 1) * rate / 16000));
    let sum = 0;
    for (let j = from; j < to && j < samples.length; j++) sum += samples[j];
    const value = Math.max(-1, Math.min(1, sum / (to - from)));
    view.setInt16(44 + i * 2, Math.round(value * (value < 0 ? 32768 : 32767)), true);
  }
  let binary = "";
  for (let i = 0; i < bytes.length; i += 8192) binary += String.fromCharCode(...bytes.subarray(i, i + 8192));
  return btoa(binary);
}

export function checkAbort(signal?: AbortSignal) { if (signal?.aborted) throw new DOMException("Voice canceled", "AbortError"); }
export function cancellable<T>(work: Promise<T>, signal?: AbortSignal): Promise<T> {
  checkAbort(signal);
  if (!signal) return work;
  return new Promise((resolve, reject) => {
    const abort = () => { signal.removeEventListener("abort", abort); reject(new DOMException("Voice canceled", "AbortError")); };
    signal.addEventListener("abort", abort, { once: true });
    work.then(resolve, reject).finally(() => signal.removeEventListener("abort", abort));
  });
}

export async function startRecording(signal?: AbortSignal): Promise<Recorder> {
  checkAbort(signal);
  if (!voiceSupported()) throw new VoiceError("unsupported", window.isSecureContext ? "This browser can't capture speech. Try the Tessera app or another browser." : "Voice needs HTTPS or the Tessera app.");
  let stream: MediaStream | undefined;
  let ctx: AudioContext | undefined;
  let source: MediaStreamAudioSourceNode | undefined;
  let node: ScriptProcessorNode | undefined;
  let mute: GainNode | undefined;
  let chunks: Float32Array[] = [], length = 0, rms = 0, done = false;
  const release = () => {
    if (done) return;
    done = true;
    signal?.removeEventListener("abort", release);
    if (node) node.onaudioprocess = null;
    source?.disconnect(); node?.disconnect(); mute?.disconnect();
    stream?.getTracks().forEach((t) => t.stop());
    void ctx?.close().catch(() => {});
    chunks = []; length = 0;
  };
  signal?.addEventListener("abort", release, { once: true });
  try {
    const opened = await cancellable(navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true } }).then(s => {
      // Permission prompts cannot be aborted; a late grant must never stay open.
      stream = s;
      if (signal?.aborted) { s.getTracks().forEach(t => t.stop()); checkAbort(signal); }
      return s;
    }), signal);
    checkAbort(signal);
    const AC = window.AudioContext ?? (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
    ctx = new AC();
    source = ctx.createMediaStreamSource(opened);
    // WebKitGTK and older Android WebViews don't all support AudioWorklet.
    node = ctx.createScriptProcessor(4096, 1, 1);
    mute = ctx.createGain(); mute.gain.value = 0;
    source.connect(node); node.connect(mute); mute.connect(ctx.destination);
    node.onaudioprocess = (e) => {
      const input = e.inputBuffer.getChannelData(0);
      let sum = 0;
      for (const value of input) sum += value * value;
      rms = Math.sqrt(sum / input.length);
      if (length < ctx!.sampleRate * 120) { chunks.push(input.slice()); length += input.length; }
    };
    await cancellable(ctx.resume(), signal);
    checkAbort(signal);
  } catch (e) {
    release();
    if ((e as DOMException)?.name === "AbortError") throw e;
    const denied = (e as DOMException)?.name === "NotAllowedError";
    throw new VoiceError(denied ? "denied" : (e as DOMException)?.name === "NotFoundError" ? "missing" : "failed", voiceError(e));
  }
  const startedAt = Date.now();
  const samples = () => { const out = new Float32Array(length); let at = 0; for (const c of chunks) { out.set(c, at); at += c.length; } return out; };
  const snapshot = () => ({ audio: wav(samples(), ctx!.sampleRate), mime: "audio/wav", ms: Math.round(length / ctx!.sampleRate * 1000) });
  return {
    startedAt,
    level: () => Math.min(1, rms * 8),
    snapshot,
    reset(prerollMs = 0) {
      const keep = Math.min(length, Math.round(ctx!.sampleRate * prerollMs / 1000));
      chunks = keep ? [samples().slice(length - keep)] : []; length = keep;
    },
    async stop() {
      if (done) throw new Error("Recording already finished");
      if (!length) { release(); throw new Error("No speech was recorded. Try again."); }
      const result = snapshot(); release(); return result;
    },
    cancel: release,
  };
}
