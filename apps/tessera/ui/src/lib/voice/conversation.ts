import type { VoiceConfig } from "../../proto/VoiceConfig";
import { api, type SessionState } from "../store";
import { compose, echo, idle, Pauses, sentence, spoken, type View } from "./flow";
import { listen, playback, support, type Input, type Playback } from "./engines";

export interface Callbacks {
  view(v: View): void;
  draft(text: string): void;
  send(text: string): Promise<void>;
  interrupt(): void;
  error(error: unknown): void;
}
export class Conversation {
  private generation = 0;
  private input?: Input;
  private inputAbort?: AbortController;
  private output?: Playback;
  private outputAbort?: AbortController;
  private partialAbort?: AbortController;
  private timer?: ReturnType<typeof setInterval>;
  private cfg?: VoiceConfig;
  private view = idle();
  private pauses = new Pauses();
  private deviceListen = false;
  private deviceSpeak = false;
  private queue: string[] = [];
  private playing = false;
  private outputGeneration = 0;
  private spokenWords = "";
  private spokenAt = 0;
  private turnBusy = false;
  private seq = 0;
  private streamId = 0;
  private consumed = 0;
  private streamText = "";
  private finalStream = "";
  private prefix = "";
  private suffix = "";
  private confirmed = "";
  private latest = "";
  private finalizing = false;
  private partialAt = 0;
  private partialBusy = false;
  private audioGeneration = 0;
  private finishing = false;
  private suppressed = false;
  constructor(private cb: Callbacks) {}
  private show(phase: View["phase"], note = "") { this.view = { ...this.view, phase, note }; this.cb.view(this.view); }
  private valid(gen: number) { return gen === this.generation && this.view.phase !== "idle"; }
  private fail(e: unknown) { this.stop(); this.cb.error(e); }

  async start(conversation: boolean, snapshot?: SessionState, prefix = "", suffix = "") {
    this.stop();
    this.inputAbort = new AbortController();
    const gen = ++this.generation;
    this.view = { ...idle(), phase: "starting", conversation, note: "Starting microphone…" }; this.cb.view(this.view);
    this.deviceListen = this.deviceSpeak = this.suppressed = false;
    this.prefix = prefix; this.suffix = suffix; this.confirmed = this.latest = "";
    this.seq = snapshot?.entries.at(-1)?.seq ?? 0;
    this.streamId = snapshot?.streaming?.startedAt ?? 0;
    this.streamText = snapshot?.streaming?.content.filter((p) => p?.type === "text").map((p) => p.type === "text" ? p.text : "").join("\n") ?? "";
    this.consumed = this.streamText.length; this.finalStream = this.streamText;
    this.turnBusy = !!snapshot && !["idle", "error", "interrupted"].includes(snapshot.info.status);
    try {
      this.cfg = await api.getVoice();
      if (!this.valid(gen)) return;
      const engine = this.cfg.engine || (this.cfg.enabled ? "service" : "off");
      if (engine === "off") throw new Error("Voice is off. Turn it on in Settings → Web & voice.");
      if (engine === "openai" && !this.cfg.openai_key) throw new Error("Save your OpenAI API key in Passwords & keys before choosing OpenAI voice.");
      const language = this.language();
      const local = engine === "auto" || engine === "device" || engine === "service" ? await support(language, this.inputAbort?.signal) : { listen: false, speak: false };
      if (!this.valid(gen)) return;
      this.deviceListen = (engine === "auto" || engine === "device") && local.listen;
      this.deviceSpeak = (engine === "auto" || engine === "device" || engine === "service") && local.speak;
      if (engine === "device" && (!this.deviceListen || (conversation && !this.deviceSpeak))) throw new Error("This device has no private speech engine for this language. Install an offline voice, or choose Automatic / On my server.");
      const serverListen = !this.deviceListen && ["auto", "server"].includes(engine);
      const serverSpeak = conversation && !this.deviceSpeak && ["auto", "server", "service"].includes(engine);
      if (this.cfg.server && ((serverListen && !this.cfg.server.listen) || (serverSpeak && !this.cfg.server.speak))) {
        this.show("starting", "Downloading private speech engines…");
        this.cfg = await api.installVoice();
        const deadline = Date.now() + 15 * 60_000;
        while (this.valid(gen) && Date.now() < deadline) {
          const s = this.cfg.server;
          if (s?.state === "failed" || s?.state === "unsupported") throw new Error(s.error || "Speech engines aren't available on this server.");
          if ((!serverListen || s?.listen) && (!serverSpeak || s?.speak) && s?.state !== "installing") break;
          await new Promise((r) => setTimeout(r, 700));
          if (!this.valid(gen)) return;
          this.cfg = await api.getVoice();
          this.show("starting", `Downloading private speech engines… ${Math.round((this.cfg.server?.done_bytes ?? 0) / 1048576)} MB`);
        }
        if (Date.now() >= deadline) throw new Error("The speech download is taking too long. Try again in Settings → Web & voice.");
      }
      if (!this.valid(gen)) return;
      this.output = playback(language, this.deviceSpeak);
      const signal = this.inputAbort?.signal;
      if (!signal || signal.aborted) return;
      const input = await listen(language, this.deviceListen, (text, final) => this.words(text, final, gen), (e) => { if (this.valid(gen)) this.fail(e); }, signal);
      if (!this.valid(gen)) { input.cancel(); return; }
      this.input = input;
      this.show("listening");
      this.timer = setInterval(() => this.tick(gen), 80);
      if (this.finishing) { const send = this.sendAfter; this.finishing = false; this.finish(send); }
    } catch (e) { if (this.valid(gen)) this.fail(e); }
  }
  private language() { const lang = this.cfg?.language; return lang ? (lang.includes("-") ? lang : lang === "en" ? "en-US" : lang) : navigator.language || "en-US"; }
  private composed(text: string) { return compose(this.prefix, text, this.suffix); }
  private draft(text: string) { this.cb.draft(this.composed(text)); }
  private words(text: string, final: boolean, gen: number) {
    if (!this.valid(gen) || !text) return;
    if (this.view.conversation) {
      if (echo(text, this.spokenWords) && (this.playing || Date.now() - this.spokenAt < 3000)) return;
      if (this.playing || this.turnBusy) this.barge();
      this.latest = text; this.draft(text);
      if (final && !this.finalizing) void this.send(text, gen);
    } else {
      this.latest = `${this.confirmed}${this.confirmed ? " " : ""}${text}`;
      this.draft(this.latest);
      if (final) { this.confirmed = this.latest; if (this.finalizing) { const send = this.sendAfter, full = this.composed(this.latest); this.finalizing = false; this.stop(); if (send) void this.cb.send(full).catch(this.cb.error); } }
    }
  }
  private tick(gen: number) {
    if (!this.valid(gen)) return;
    const recorder = this.input?.recorder;
    if (!recorder || this.finalizing) return;
    const now = Date.now(), level = recorder.level();
    if (!this.view.conversation && now - recorder.startedAt >= 120_000) { this.finish(); return; }
    this.view.level = level; this.cb.view({ ...this.view });
    const vad = this.pauses.tick(level, now);
    if (this.view.conversation && this.playing && vad.interrupt && now - this.spokenAt > 650) this.barge();
    if (this.view.conversation && vad.finish) { void this.transcribe(true, gen); return; }
    if (!this.pauses.heard && !this.playing) recorder.reset(300);
    // Live words only on a fast, private server; never duplicate paid requests.
    if (this.cfg?.server?.live && ["auto", "server"].includes(this.cfg.engine) && this.pauses.heard && !this.playing && !this.partialBusy && now - this.partialAt > 2500) {
      this.partialBusy = true; this.partialAt = now;
      const clip = recorder.snapshot(), audioGen = this.audioGeneration;
      const abort = this.partialAbort = new AbortController();
      void api.transcribe(clip.audio, clip.mime, "server", true, abort.signal).then((r) => { if (this.valid(gen) && audioGen === this.audioGeneration && !this.finalizing) this.draft(r.text); }).catch(() => {}).finally(() => { if (this.partialAbort === abort) { this.partialBusy = false; this.partialAbort = undefined; } });
    }
  }
  finish(send = false) {
    if (this.view.phase === "starting") { this.finishing = true; this.sendAfter = send; return; }
    if (this.view.phase === "idle" || this.finalizing) return;
    this.sendAfter = send;
    if (this.input?.recorder) void this.transcribe(true, this.generation);
    else {
      this.finalizing = true;
      this.input?.finish();
      // A recognizer may end without a final result (e.g. silence).
      const gen = this.generation;
      setTimeout(() => { if (this.valid(gen) && this.finalizing) { const heard = this.latest.trim(), text = this.composed(this.latest); this.stop(); if (send && heard) void this.cb.send(text).catch(this.cb.error); } }, 1200);
    }
  }
  private sendAfter = false;
  private async transcribe(final: boolean, gen: number) {
    const recorder = this.input?.recorder;
    if (!recorder || this.finalizing || !this.valid(gen)) return;
    this.partialAbort?.abort(); this.partialAbort = undefined; this.partialBusy = false;
    this.finalizing = true; this.audioGeneration++;
    this.show("sending", "Turning speech into text…");
    this.pauses.reset();
    try {
      // Dictation is finished before even starting a slow server request.
      // Conversation alone keeps capture open to allow interruptions.
      const clip = this.view.conversation ? recorder.snapshot() : await recorder.stop();
      if (!this.valid(gen)) return;
      if (this.view.conversation) recorder.reset();
      const r = clip.ms >= 350 ? await api.transcribe(clip.audio, clip.mime, this.cfg?.engine === "auto" ? "server" : undefined, !final, this.inputAbort?.signal) : { text: "" };
      if (!this.valid(gen)) return;
      this.finalizing = false;
      const text = r.text.trim();
      if (this.view.conversation) {
        if (text && !(echo(text, this.spokenWords) && (this.playing || Date.now() - this.spokenAt < 8000))) { this.barge(); this.draft(text); await this.send(text, gen); }
        else this.show(this.playing ? "speaking" : this.turnBusy ? "waiting" : "listening");
      } else {
        const send = this.sendAfter;
        if (text) this.draft(text);
        const full = this.composed(text);
        this.stop();
        if (send && text) await this.cb.send(full);
      }
    } catch (e) { if (this.valid(gen)) this.fail(e); }
  }
  private async send(text: string, gen: number) {
    this.finalizing = true;
    this.show("sending");
    this.spokenWords = "";
    this.queue = []; this.suppressed = false;
    try {
      await this.cb.send(this.composed(text));
      if (this.valid(gen)) { this.prefix = this.suffix = ""; this.latest = ""; this.finalizing = false; this.show("waiting"); }
    } catch (e) { if (this.valid(gen)) this.fail(e); }
  }
  private barge() {
    this.suppressed = true;
    const wasBusy = this.turnBusy;
    this.outputAbort?.abort(); this.outputAbort = undefined;
    this.outputGeneration++; this.output?.cancel(); this.queue = []; this.playing = false;
    this.finalStream = this.streamText; this.consumed = this.streamText.length;
    if (wasBusy) this.cb.interrupt();
    this.turnBusy = false;
    this.show("listening");
  }
  update(s?: SessionState) {
    if (!s || !this.view.conversation || this.view.phase === "idle" || this.view.phase === "starting") return;
    this.turnBusy = !["idle", "error", "interrupted"].includes(s.info.status);
    const stream = s.streaming;
    if (this.suppressed) { this.seq = s.entries.at(-1)?.seq ?? this.seq; return; }
    if (stream) {
      const text = stream.content.filter((p) => p?.type === "text").map((p) => p.type === "text" ? p.text : "").join("\n");
      if (stream.startedAt !== this.streamId) { this.streamId = stream.startedAt; this.consumed = 0; this.streamText = ""; }
      this.streamText = text;
      this.feed(text, stream.ended);
      if (stream.ended) this.finalStream = text;
    }
    // Read only the new tail, not all of a long chat's history on each delta.
    const fresh = [];
    for (let i = s.entries.length - 1; i >= 0 && s.entries[i].seq > this.seq; i--) fresh.push(s.entries[i]);
    for (const entry of fresh.reverse()) {
      this.seq = entry.seq;
      if (entry.kind !== "assistant") continue;
      const text = entry.content.filter((p) => p.type === "text").map((p) => p.type === "text" ? p.text : "").join("\n");
      if (text === this.finalStream || text === this.streamText || (this.streamText && text.startsWith(this.streamText))) { this.feed(text, true); this.finalStream = ""; }
      else { this.consumed = 0; this.feed(text, true); }
    }
    if (!this.turnBusy && !this.playing && !this.queue.length && !this.finalizing) this.show("listening");
  }
  private feed(text: string, final: boolean) {
    let part;
    while ((part = sentence(text.slice(this.consumed), final))) {
      this.consumed += part.used;
      const words = spoken(part.text);
      if (words) this.queue.push(words);
    }
    void this.drain();
  }
  private async drain() {
    if (this.playing || !this.queue.length || !this.output) return;
    this.playing = true;
    const gen = this.generation, outGen = this.outputGeneration;
    try {
      while (this.queue.length && this.valid(gen) && outGen === this.outputGeneration) {
        const text = this.queue.shift()!;
        this.show("speaking");
        const abort = this.outputAbort = new AbortController();
        const audio = this.deviceSpeak ? undefined : await api.speak(text, this.cfg?.engine === "auto" || this.cfg?.engine === "service" ? "server" : undefined, abort.signal);
        if (!this.valid(gen) || outGen !== this.outputGeneration) return;
        this.spokenWords = text; this.spokenAt = Date.now(); this.pauses.reset(); this.input?.recorder?.reset();
        await this.output.play(text, audio);
      }
      if (this.valid(gen) && outGen === this.outputGeneration) { this.playing = false; this.show(this.turnBusy ? "waiting" : "listening"); this.spokenAt = Date.now(); }
    } catch (e) { if (this.valid(gen) && outGen === this.outputGeneration) this.fail(e); }
  }
  stop() {
    this.generation++; this.outputGeneration++; this.audioGeneration++; clearInterval(this.timer); this.timer = undefined;
    this.outputAbort?.abort(); this.outputAbort = undefined;
    this.partialAbort?.abort(); this.partialAbort = undefined;
    this.inputAbort?.abort(); this.inputAbort = undefined;
    this.input?.cancel(); this.input = undefined; this.output?.cancel(); this.output = undefined;
    this.queue = []; this.playing = this.finalizing = this.partialBusy = this.finishing = this.sendAfter = false;
    this.pauses.reset(); this.spokenWords = ""; this.view = idle(); this.cb.view(this.view);
  }
}
