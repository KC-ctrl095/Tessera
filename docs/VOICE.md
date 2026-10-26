# Voice

Talk to any chat and hear it answer. Your words become text in the message
box (you can check them before they go), and in a **voice conversation** the
answer is read out to you and Tessera listens again, like a phone call. Talk
over it to interrupt.

Voice is free and private by default: speech is turned into text, and text
into speech, on your own device or your own server. Nothing goes to a cloud
service unless you pick one.

## Using it

- **Dictate**: tap the microphone in the message box, speak, tap it again
  (or press Enter). Review the words in the box before sending. Private device
  engines, and fast servers, show live words. Or hold the microphone while
  you speak and let go to send at once.
- **Voice conversation**: when the message box is empty, the button where
  Send usually is starts a conversation (the waves icon). Speak; when you
  pause, your words are sent to the chat's model. The answer is read out
  while it is written. Silence ends a recorded turn after 800 ms. Sustained
  speech (320 ms) interrupts playback and the pending agent answer. Tap **End**
  (or press Escape) to stop voice. Work already sent continues unless you Stop it.
- While the microphone is on, the message box shows a red dot (and an input
  level for server capture). The microphone is only ever on after you tapped
  it, and it turns off when you end voice, close or switch chats, disconnect,
  or put the app away. Starting voice in another tile ends the first one.

## Where speech is handled
- **OpenRouter (your key)** explicitly sends speech to OpenRouter and its
  speech providers. Uses `openai/gpt-4o-mini-transcribe` and
  `elevenlabs/eleven-v4-turbo` with Sarah by default. The settings offer 21
  stock voices and a cancellable, short paid preview. It reuses the key of a
  provider whose address is exactly `https://openrouter.ai/api/v1` (optional
  trailing slash or explicit port 443), or `OPENROUTER_API_KEY` saved in
  Passwords & keys. A provider's display name alone is never enough. Neither
  the credential nor a provider-supplied endpoint reaches the client. If private
  recognition is missing or the server is slow, settings recommend this option
  without changing Automatic or sending any audio.

**Settings → Web & voice → Voice** has one choice:

| Choice | What happens |
|---|---|
| **Automatic** (recommended) | Your device's own speech engine when it has a private one; otherwise your server's. Never a cloud service. |
| **On this device** | Only the phone's or computer's own engine (each device uses its own). |
| **On my server** | Your Tessera server turns speech into text and back. The first use downloads the speech engines (about 100 MB, once). |
| **OpenAI (your key)** | OpenAI's transcription and speech services, with the API key you saved in Passwords & keys (or Models). Paid by you, per minute. Your chosen chat model still answers; this is not Realtime, and ChatGPT plan sign-ins do not pay for speech. |
| **OpenRouter (your key)** | Paid cloud transcription and speech through OpenRouter. Your chosen chat model still answers. |
| **Off** | Voice controls offer an explicit setup choice, without opening the mic. |

Under **More** you can choose the server's voice and speech model, language,
keep recordings and automatic sending after dictation. **Another transcription
service** explicitly selects Groq or a Whisper server on a stronger computer;
audio goes only to that chosen service. Its API key is write-only.

What "on this device" can do depends on the device:

- **Android app**: the phone's on-device speech recognizer (Android 12 and
  later) and its text-to-speech voices that work offline.
- **Desktop app and browsers**: speech output uses the computer's own voices.
  Speech input on the device needs a browser that recognizes speech locally
  (recent Chrome can); browsers that would send your voice to their maker's
  cloud are not used. Elsewhere, Automatic uses your server.

## Your server's engines

- Speech to text: [whisper.cpp](https://github.com/ggml-org/whisper.cpp) with
  OpenAI's Whisper model (*tiny* on slower computers, *base* on faster ones).
- Text to speech: [Piper](https://github.com/rhasspy/piper) with a voice for
  your language (English, British English, Dutch, German, French, Spanish;
  each voice about 60–75 MB, downloaded when you choose it).
- They are downloaded from their official releases and checked against a
  fixed fingerprint (SHA-256) before use, into your server's data folder
  (`voice/`). Computers include Linux (x64, ARM64) and Windows (x64). macOS uses a
  `whisper-cli` you installed (Homebrew) plus Piper.
- They run at the lowest priority, so chats and other devices stay responsive.
  On an older processor (no AVX) a sentence takes a few seconds to recognize;
  the live words while you speak are only shown where the engine is fast
  enough.
- Downloads, scratch audio, kept recordings and audio-producing engines keep
  20 GiB free on the data disk, counting their output space. Stop cancels the
  current transcription or speech operation without disconnecting the chat.
  Engines have one deadline for input, output and exit, a 2 GiB memory limit,
  bounded output and temporary files. Each clip is at most two minutes and
  16 MiB. Finished dictation releases the microphone before transcription;
  a voice conversation keeps listening so you can interrupt.

## Privacy

- Tessera does not keep your voice by default. Audio is handled in memory (or a private
  temporary file that is deleted right away). It never reaches a chat's history
  or logs. Private engines keep audio local. A cloud speech choice sends it to
  the chosen speech provider, not to the chat model. Cloud providers have their
  own data handling policies. Only text you send is kept in Tessera's chat history.
  If you turn on **Keep recordings**, they are saved in your server's
  data folder (`voice/recordings`) and can be deleted there with one button.
- Spoken answers are made from the chat's text and are not stored either.
- API keys stay on your server and are never shown again.
- Service addresses cannot include URL credentials or query parameters. Put
  the key in its separate API key field. Non-local services require HTTPS,
  checked when selected or its address changes, and before every speech
  request, including reused model-provider addresses. An unchanged inactive
  service can stay saved while you turn voice off or choose another engine,
  even if that service is unavailable. Plain HTTP is limited to loopback,
  private LAN, link-local and IPv6 unique-local addresses. Single-label and `.local` names
  must resolve only to those addresses; the checked addresses are pinned for
  the connection. Speech requests do not follow redirects.
- Provider replies are read in bounded chunks, at most 64 KiB for transcript
  JSON, 16 KiB for errors and 16 MiB for spoken audio. Malformed or oversized
  replies are not used as transcripts or spoken audio. Retained recordings
  use a private atomic temporary writer with reserve checks before and during
  writing and before commit; failures remove the temporary recording.

## How it works (for developers)

- The client owns the conversation (`apps/tessera/ui/src/lib/voice/`, loaded
  only when voice is first used): a small state machine
  (`idle → listening → sending → waiting → speaking → listening…`), the
  engines below, sentence splitting of the streamed answer, and barge-in.
- Engines, per direction: device (Web Speech with on-device recognition only;
  `speechSynthesis` with local voices only; the Android app's native
  `SpeechRecognizer.createOnDeviceSpeechRecognizer` / `TextToSpeech` through
  the app's own commands) or server. For the server the client records
  16 kHz mono WAV itself (no codec on the server needed), detects pauses
  from the input level, and sends `transcribe` (`partial: true` for live words
  while speaking, when `VoiceConfig.server.live`). Paid cloud choices make one
  final transcription request, never repeated paid live-word requests.
- Requests: `get_voice`/`save_voice` (the choice, `VoiceConfig.engine`),
  `install_voice` (downloads in the background; progress in `get_voice`),
  `transcribe` (`engine` picks server/openai/openrouter/service), `speak` (text →
  `SpeechAudio`, WAV from Piper or MP3 from cloud speech), `delete_voice_recordings`.
- Barge-in: while an answer is spoken the microphone keeps listening. Device
  transcript matching filters playback echoes; server capture uses sustained
  speech after echo cancellation to stop playback, then filters repeated
  answer text before sending. Speaker/room echo handling is best effort, not
  a guarantee; headphones give the most reliable interruptions.
- The first complete streamed sentence is spoken before the agent turn ends.
  Cloud speech prepares at most one following sentence during playback. Stop
  and barge-in cancel both current speech and this lookahead. Later text is not
  pre-synthesized. A short local listening tone and optional haptic mark capture
  start. The tone is cleared from recorded input. Platform echo cancellation and
  a 250 ms playback tail guard help prevent loops. Playback-only noise is never
  transcribed.

## Not yet (phase 2)

- Running the server engines on your strongest paired computer (a GPU
  machine) automatically. Today: point **More → Another service** at a Whisper
  server there.
- OpenAI Realtime (speech to speech, lowest delay; needs an API key and skips
  your chosen chat model), and true streaming recognition on the server.
- More voices and languages, and choosing a voice per bot.

## One-tap setup

`createVoiceHost(...).start(true)` remains the assistant-entry API. If voice is
explicitly off it opens an inline **Voice is off** card without opening the mic.
**Use OpenRouter (your key)** appears only when the server reports a key and
plainly says it is paid cloud speech. **On this device** and **On my server** are
private alternatives. Choosing one saves that explicit selection and immediately
resumes the original conversation. **Not now**, Escape, hiding the app, or changing
servers dismisses it. `enableVoice(engine)` is exported for other explicit setup
actions, not automatic fallback. Empty legacy `engine` plus `enabled=false`
currently normalizes to Automatic, not Off. Existing private defaults are unchanged.

## OpenRouter selection and cost

Verified against the live catalog and official audio guides on 2026-10-25.
Prices can change. Chat-model use is separate.

| Engine/model | Approximate audio cost | Decision |
| --- | --- | --- |
| ElevenLabs v4 Turbo | $0.04 per 1,000 text characters, roughly $0.04 per spoken minute at 1,000 characters/minute | Default output, natural stock voices and MP3 |
| GPT-4o mini transcribe | About $0.003/minute, audio/text token metering | Default input, good latency in the short probe |
| Whisper large v3 Turbo | $0.0001998/minute | Lower-cost alternative, slower in the short probe |
| ElevenLabs Scribe v2 | $0.0036667/minute | Similar short-probe latency to mini transcribe |
| Whisper-1 | $0.006/minute | Older alternative |
| GPT-4o transcribe | About $0.006/minute, token metering | Higher-cost alternative |
| Google Gemini 3.8 Flash Lite TTS | $0.50/M input tokens and $6/M audio output tokens, about $0.009/minute at 25 audio tokens/sec plus input | PCM-only in the catalog, needs WAV packaging |
| OpenAI mini TTS | Not available in the live OpenRouter speech catalog | A tiny request was rejected, not chosen |
| Device / server | No per-minute cloud fee | Private, availability and speed depend on hardware |

OpenRouter accepts multipart transcription, up to 25 MB and approximately 30
minutes. Tessera's UI records at most two minutes. The cloud admission cap is
25 MiB, local engine audio is capped at 16 MiB, and STT JSON at 64 KiB.
The STT endpoint returns JSON, not streaming words. For speech, the guide exposes
`stream:true` SSE with base64 `audio.delta` events. Tessera deliberately requests
non-SSE MP3 and buffers only a sentence under the existing 16 MiB response limit.
ElevenLabs v4 Turbo advertises MP3, PCM, Opus, ulaw and alaw. Stock voice IDs are
lowercase. Google currently advertises only PCM and case-sensitive voices such
as `Kore`. Unwrapped PCM is not sent to Android WebView playback.

Requests use the fixed HTTPS OpenRouter host with redirects disabled, TLS
verification, bounded chunked/error bodies and connection-scoped cancellation.
Errors scrub credentials and URLs. `openrouter_key` is a read-only capability,
not a credential. Cloud partial transcription is rejected before dispatch.

## Current latency evidence

The server accepts an optional per-request device language hint. A saved language
always wins, and the hint is not persisted. The UI supplies the device's language
when no preference was saved, then the server normalizes it to the primary ISO
code. This avoids Whisper's costly language-detection pass on English input.
Older clients without the hint keep automatic detection.

Short synthetic phrase, single requests from a test server on 2026-10-25. The
recorded OpenRouter speech result was 2.84 seconds of audio. These are component
timings, not a claim about any phone or a full real-agent turn.

| Component | Complete response latency |
| --- | --- |
| Server tiny Whisper + Piper, automatic language | 6.316 / 5.897 / 6.248 s total, with 800 ms endpoint pause |
| Server tiny Whisper + Piper, device `en-US` hint | 3.591 / 4.111 / 3.740 s total, with 800 ms endpoint pause |
| OpenRouter ElevenLabs v4 Turbo MP3 | 2.017 s |
| OpenRouter Google Flash Lite PCM | 2.111 s |
| OpenRouter GPT-4o mini transcription | 0.683 s |
| OpenRouter Whisper large v3 Turbo transcription | 1.685 s |
| OpenRouter Scribe v2 transcription | 0.686 s |

The server trials used a real encrypted throwaway server and synthetic Piper
input, fixed response text and no chat-model wait. Median total dropped from
**6.248 s to 3.740 s**, about 40 percent, by avoiding language detection. Median
STT dropped from 4.003 s to 1.384 s. The hinted input retained the full greeting
in all three trials, whereas automatic detection lost "Hello there". This is a
small synthetic result, not a general accuracy score.

Two additional fixed-brain cloud pipeline trials, using local Piper-generated
input and an 800 ms endpoint pause, reached buffered MP3 in **4.777 s** and
**2.626 s**. Their STT times were 1.926 s and 0.764 s. TTS first actual payload
arrived in 1.948 s and 0.926 s, with complete buffers in 1.956 s and 0.932 s.
This variability matters more than SSE framing for a very short sentence.
The trials intentionally omit chat-model latency and phone playback overhead.

The initial samples imply an illustrative 3.50 s floor from actual end of speech
to buffered playback using these two component samples and the 800 ms endpoint
pause, plus chat first-sentence latency, upload and WebView playback overhead.
Previously the pause alone was 1,050 ms. This is an arithmetic estimate, not an
end-to-end measurement. The one-sentence lookahead mainly improves later sentence
gaps. Actual on-device recognition and Android audio latency require a phone test.

## ChatGPT-plan realtime research

The official unmodified Codex npm package 0.164.0 exposes experimental
`thread/realtime/start`, `appendAudio`, `appendText`, `appendToolResponse` and
`stop`, plus output-audio, transcript, handoff and error notifications. Its
schema supports V3, websocket and WebRTC transports, audio or text output,
`clientManagedHandoffs` and role-bearing initial items. This is not a stable
OpenAI voice API contract.

A tiny ephemeral-thread probe used only `codex login status` and official
app-server messages. The CLI reported a ChatGPT login. `thread/realtime/start`
acknowledged the request, then emitted **realtime conversation requires API key
auth** on the websocket transport. No audio was produced. A separate official
WebRTC start reached `chatgpt.com/backend-api/codex/realtime/calls` through the
CLI's own authentication, but returned HTTP 401, **Could not parse your
authentication token. Please try signing in again.** No SDP answer or audio
arrived. Research stopped instead of asking for sign-in or touching auth state.
No auth files or tokens were read, copied or moved by the researcher.
Do not treat the initial RPC acknowledgement as success. WebRTC's plan-auth path
exists in the official source, unlike the API-key-only websocket path, but is
unverified with a fresh sign-in. Public Codex pricing
lists subscription usage and limits, but does not document realtime voice minute
allowances or a guaranteed voice entitlement. No plan price or quota claim is made.

Recommended next design, only when official ChatGPT authentication actually works
and the billing terms are clear

1. Keep an official app-server child server-side, gated by a user-selected engine.
2. Keep Tessera as the brain. Give realtime only a narrow forwarding tool or V3
   handoff that sends a transcript to the current Tessera conversation.
3. Preserve normal Tessera permissions and approvals. The voice model must not
   gain shell/tools independently. `clientManagedHandoffs` alone needs a careful
   protocol audit because Codex can also start a backing-agent turn.
4. Return streamed Tessera sentences via documented tool responses/handoff appends.
   Stop/barge-in cancels both paths and clears queued audio.
5. Forward only public audio/transcript events to authenticated clients. Never
   expose the CLI's backend credentials, auth files or tokens.

This is not implemented. OpenRouter is the usable explicit paid route today.

### Primary sources

- https://openrouter.ai/docs/guides/overview/multimodal/tts
- https://openrouter.ai/docs/guides/overview/multimodal/stt
- https://openrouter.ai/api/v1/models?output_modalities=speech
- https://openrouter.ai/api/v1/models?output_modalities=transcription
- https://openrouter.ai/blog/announcements/announcing-audio-apis/
- https://github.com/openai/codex/blob/rust-v0.164.0/codex-rs/app-server/README.md
- https://github.com/openai/codex/blob/rust-v0.164.0/codex-rs/app-server-protocol/src/protocol/v2/realtime.rs
- https://github.com/openai/codex/blob/rust-v0.164.0/codex-rs/core/src/realtime_conversation.rs
- https://developers.openai.com/codex/pricing
