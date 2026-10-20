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
  while it is written. Start talking to interrupt it (this stops that chat's
  current work too). Tap **End** (or press Escape) to stop voice; work already
  sent to the chat continues unless you use the chat's Stop button.
- While the microphone is on, the message box shows a red dot (and an input
  level for server capture). The microphone is only ever on after you tapped
  it, and it turns off when you end voice, close or switch chats, disconnect,
  or put the app away. Starting voice in another tile ends the first one.

## Where speech is handled

**Settings → Web & voice → Voice** has one choice:

| Choice | What happens |
|---|---|
| **Automatic** (recommended) | Your device's own speech engine when it has a private one; otherwise your server's. Never a cloud service. |
| **On this device** | Only the phone's or computer's own engine (each device uses its own). |
| **On my server** | Your Tessera server turns speech into text and back. The first use downloads the speech engines (about 100 MB, once). |
| **OpenAI (your key)** | OpenAI's transcription and speech services, with the API key you saved in Passwords & keys (or Models). Paid by you, per minute. Your chosen chat model still answers; this is not Realtime, and ChatGPT plan sign-ins do not pay for speech. |
| **Off** | No microphone button anywhere. |

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
  (`voice/`). Computers: Linux (x64, ARM64), Windows (x64); macOS uses a
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

- Your voice is never stored: the audio is handled in memory (or a private
  temporary file that is deleted right away) and never reaches a chat's
  history, the logs or any model. Only the text you send is kept, like typed
  text. If you turn on **Keep recordings**, they are saved in your server's
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
  while speaking, when `VoiceConfig.server.live`).
- Requests: `get_voice`/`save_voice` (the choice, `VoiceConfig.engine`),
  `install_voice` (downloads in the background; progress in `get_voice`),
  `transcribe` (`engine` picks server/openai/service), `speak` (text →
  `SpeechAudio`: WAV from Piper, MP3 from OpenAI), `delete_voice_recordings`.
- Barge-in: while an answer is spoken the microphone keeps listening. Device
  transcript matching filters playback echoes; server capture uses sustained
  speech after echo cancellation to stop playback, then filters repeated
  answer text before sending. Speaker/room echo handling is best effort, not
  a guarantee; headphones give the most reliable interruptions.

## Not yet (phase 2)

- Running the server engines on your strongest paired computer (a GPU
  machine) automatically. Today: point **More → Another service** at a Whisper
  server there.
- OpenAI Realtime (speech to speech, lowest delay; needs an API key and skips
  your chosen chat model), and true streaming recognition on the server.
- More voices and languages, and choosing a voice per bot.
