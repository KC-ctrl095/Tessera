# Web-view sound: deferred lightweight design

Web views deliberately keep `--mute-audio`. Frames are remote pictures. Unmuting the
server speaker would not send sound to a phone and could expose a lecture or
private call to other people. The tile has an explicit **Audio / video in your
browser** action. It opens the same address in the user's device browser; that
separate profile may need its own sign-in.

## A viable opt-in path

1. Start a private audio sink **only while one browser has an attached device
   that explicitly requested sound**. On headless Linux this requires a private
   PulseAudio-compatible server or PipeWire instance, not the user's default
   sink/monitor. Socket and runtime directory are mode 0700 in that browser's
   protected profile. Chrome alone inherits its address; no host microphone or
   other application's sound can be captured. Stop/revoke kills this scope.
2. Capture just that sink and Opus-encode stereo at about 48–64 kbit/s, 20 ms
   packets. Use the existing pinned encrypted device channel, a bounded queue
   of about 120 ms, monotonic timestamps and a discontinuity marker. Do not
   base64 whole audio recordings or write them to disk. Drop late packets.
3. Device playback is a lazy Web Audio/AudioWorklet module, created from a user
   gesture to obey autoplay rules. One mute button per visible tile; a hidden
   tile/background app immediately unsubscribes. No subscribed devices means
   destroy encoder/sink, not an always-running daemon. Disconnection, Pause,
   Stop and browser generation checks cancel stale packets.
4. Chrome audio is browser-process-scoped, not automatically page-scoped.
   Therefore initially allow only **one audible page per browser host**, mute
   all others explicitly and freeze them. Never deliver background sign-in
   windows or another chat's audio to a subscriber. A future per-page native
   capture API can relax this only with measured isolation tests.
5. Windows/macOS need equivalent isolated browser-only capture (not a loopback
   of the user's desktop). An unavailable backend must stay muted and show the
   honest external-browser option; no permission loosening or fallback to
   system-wide capture.

## Why it is not shipped

The current protocol transports page frames only. CDP supplies no general
per-tab encoded audio stream. A private in-memory display does not
supply a private audio service, and installing a system-wide audio daemon or
capturing desktop loopback would violate the task's isolation and lightweight
requirements. Merely adding ffmpeg/Opus plus a default monitor is not safe:
it can include unrelated applications/pages and adds persistent processes.
A new optional native capture backend, explicit per-viewer permissions and
cross-platform playback need their own isolated prototypes and measurements.

Acceptance for that follow-up: silence/no extra process when hidden/muted,
no microphone/desktop/other-tab capture, Stop/generation fencing, bounded
CPU/RAM/queue, <64 kbit/s nominal stream, interruption without queued playback,
Android trusted-gesture playback, and Linux/Windows negative isolation tests.
This is a design, **not an implemented or tested audio path**.
