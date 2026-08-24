# ARD P5 Native Playback and Windows Microphone Design

**Status:** Superseded by `ard_re/P5_PROTOCOL_ANALYSIS.md`; implementation forbidden
**Date:** 2026-08-23
**Scope:** P5 PC-to-Mac audio over the unmodified password-authenticated HPSS session

> Stock `ScreenSharing.framework` now proves that Audio Chat is gated by IDS or
> an Apple-ID invitation address. The project forbids that identity path, and
> no generic application-readable input device was recovered. Keep this file as
> historical design context only; do not resume its tone or microphone tasks.

## Objective

Make the opt-in FreeRemoteDesk path carry live audio from the Windows default
input device to native playback on the connected Mac:

```text
Windows microphone
  -> bounded mono 48 kHz PCM frames
  -> AAC-ELD
  -> RFC 3640 single-AU payload
  -> RTP/SRTP over the negotiated HPSS audio socket
  -> stock ScreensharingAgent / AVConference
  -> Mac native audio output
```

Completion requires intelligible microphone audio on the Mac.  Local capture,
encoding, UDP delivery, or an authenticated reception report alone is not
completion.

## Binding Product and Security Constraints

- FreeRemoteDesk remains a remote-login client.  Do not install or run a custom
  Mac application, daemon, launch agent, driver, plugin, relay, proxy, or test
  fallback.
- Use only the Mac username and password already used by the encrypted remote
  login session.  Do not request, store, or use Apple ID, iCloud, IDS, APNs, or
  QuickRelay credentials.
- The Mac remains unmodified.  Stock `screensharingd`, `ScreensharingAgent`,
  `avconferenced`, and their existing private frameworks may be observed with
  built-in, non-privileged commands.
- Do not use Frida, `sample`, `sudo`, downloaded agents, or injected code on the
  Mac.
- Never place target identifiers, usernames, passwords, executable paths, media
  keys, or raw unredacted remote output in source, documentation, captures, test
  names, or command-line arguments.
- `--udp-audio-input` remains explicit and requires `--udp-media`.
- P5 failure is isolated.  Video, control, P3, P4, and session teardown must
  continue.
- The repository default `D:\FreeRemoteDesk\target` remains read-only.  Cargo
  commands use the approved isolated C-drive target with incremental and debug
  output disabled.
- Do not initialize Git in this checkout.

## Corrected Evidence Baseline

### Verified transport and endpoint evidence

- Password-authenticated stock HPSS reached Active mode 4
  (`RemoteMicrophone`).
- The reviewed deterministic probe sent exactly 500 packets for one recorded
  SSRC and extended sequence range.
- Authenticated, replay-accepted SRTCP from the stock endpoint named the same
  SSRC, reported an extended highest sequence inside the sent range, and
  reported zero cumulative loss.
- This proves endpoint reception/reporting.  It does not prove every packet was
  received, decoded, or played.

### Invalidated output-frequency gate

The local current `ScreensharingAgent` binary has SHA-256
`D1D38DC66E4D8A917201BF04816A6DC34CB34E0DE77BA36885586F3F944E0791`.
Read-only Ghidra analysis established:

- `-[SSUDPSender start]` at `0x10000c4c4` starts both video streams and then
  sends `start` to `audioStream`.
- `-[SSUDPSender stream:updateInputFrequencyLevel:]` at `0x10000f4a9` and
  `-[SSUDPSender stream:updateOutputFrequencyLevel:]` at `0x10000f5a8` only log
  the callback and data length.
- The `ScreensharingAgent` image contains neither
  `setInputFrequencyMeteringEnabled:` nor
  `setOutputFrequencyMeteringEnabled:`.  The AVConference framework provides
  those APIs, but the stock caller does not enable them.
- The old live helper's `remote-avconference-log` predicate explicitly excluded
  every event containing `Health`, which can hide receiver, speaker callback,
  playback, and Audio I/O counters.

Therefore the absence of `updateOutputFrequencyLevel ... data length N` is not
evidence that playback failed.  That callback is diagnostic-only and must never
again be a mandatory gate.

### Still unverified

- The 500-frame deterministic tone was not accompanied by a recorded user
  audibility result.
- The previous harness terminated the viewer after its bound, so clean viewer
  teardown was not proved.
- Windows microphone capture has not yet been connected to the P5 viewer
  runtime.

## Architecture

The repair is split into two evidence-separated cutovers.  A deterministic
tone first proves the existing codec/packet/playout path without involving a
capture device.  Only a positive tone checkpoint permits the production path
to open the Windows microphone.

### 1. Transport and crypto ownership

`media_transport.rs` remains the sole owner of:

- the generation-bound audio socket;
- outbound RTP SSRC, sequence, timestamp, rollover, SRTP protection, and send;
- authenticated SRTCP parsing and replay acceptance;
- the actual sent range and typed reception evidence.

The viewer never re-parses RTCP and never treats unauthenticated logs or UDP
arrival as transport confirmation.

### 2. Probe and microphone phase ownership

`audio_input.rs` owns the device-independent state machine:

```text
Disabled
  -> Negotiating
  -> ProbeReady
  -> ProbeSending
  -> ProbeAwaitingReport
  -> ProbeConfirmed
  -> MicrophoneReady
  -> MicrophoneStreaming

Any active phase --local P5 error--> Degraded
Any phase --generation change / teardown--> Disabled
```

- The probe is exactly 500 mono frames and is never an automatic fallback.
- `ProbeConfirmed` means authenticated in-range SRTCP only; it does not mean
  audible playback.
- Promotion to `MicrophoneReady` is a reviewed-build decision after a separate
  positive live tone checkpoint, not an automatic same-run transition.
- The production microphone run uses the first 500 successfully sent access
  units as its confirmation window and latches an early valid report.

### 3. Codec ownership

`audio_codec.rs` retains the recovered Apple-compatible contract:

- signed 16-bit mono PCM;
- 48,000 Hz;
- 480 samples per access unit;
- AAC-ELD without SBR;
- the verified mono AudioSpecificConfig;
- one RFC 3640 access unit per RTP packet, payload type 101.

The deterministic tone must be locally decoded in a test and shown to contain
non-silent samples before any live probe build is reviewed.

### 4. Windows capture ownership

`audio_io.rs` owns `AudioCapture` and PCM conversion.  The viewer may open the
default input device only when all of the following are true:

- the user explicitly selected `--udp-audio-input` together with
  `--udp-media`;
- password-authenticated Message 2 was validated as mode 4;
- the current generation's audio transport is Active;
- the deterministic tone checkpoint has been recorded as positive in the SDD
  ledger and the production guard change has passed independent review.

Capture output is converted into exact protocol frames and stored in a bounded
queue.  When full, the queue drops the oldest local frame to bound latency.
Capture open/read/device-removal errors degrade P5 once and release the device;
they do not terminate the viewer.

### 5. Viewer coordination

`hpss_viewer.rs` coordinates but does not own crypto, codec layout, or device
conversion.  Each reader tick has bounded audio work so microphone traffic
cannot starve TCP control, video receive, rendering, or input.

The temporary probe build and the production microphone build are separate
reviewed artifacts.  The production guards remain closed until the tone gate
passes.  No executable containing an unreviewed fallback is eligible for live
use.

### 6. CLI ownership

`main.rs` keeps the default Mac-to-PC flow unchanged.  After the positive tone
checkpoint, `--udp-audio-input --udp-media` selects the typed PC-to-Mac flow.
Invalid flag combinations fail before address parsing, credential access,
session construction, capture, or networking.

## Corrected Live Evidence Collection

The helper will add a narrowly scoped `remote-avconference-playback-log` mode
using stock `/usr/bin/log show`.  It must redact using the existing credential
provider and collect only events relevant to:

- `AVCAudioStream`, `VCAudioReceiver`, or `VCAudioPlayer` startup;
- receiver packet/decode/playback failures;
- `speakerProcsCalled`, playback counts, and Audio I/O health;
- output-device or speaker setup failures;
- RTP/SRTP authentication, parsing, and timeout failures.

Unlike the old query, it must not exclude all `Health` events.  It must select
only the named audio fields to keep artifacts bounded and avoid unrelated
system data.  Frequency-meter callbacks may be captured if naturally emitted,
but their absence is neutral.

## Acceptance Gates

### Automated gate before any live run

All must pass:

1. mode 4 and mode 8 offers/answers remain distinct and fixture-valid;
2. the mono encoder configuration, frame length, channel count, payload type,
   RFC 3640 header, RTP timestamp increment, and SRTP keys match verified
   fixtures;
3. a generated probe frame round-trips through the local decoder and is
   measurably non-silent;
4. authenticated in-range SRTCP confirms only the matching generation, SSRC,
   and sent range; wrong, malformed, replayed, or out-of-range reports do not;
5. capture remains unopened in Disabled, Negotiating, and probe phases;
6. per-tick probe/microphone work and capture queues are bounded;
7. generation reset and teardown drop encoder, capture, queued PCM, sent-range,
   and confirmation state;
8. P5 degradation leaves video, control, P3, P4, and teardown active;
9. formatting, default/no-default tests, default/no-default builds, and both
   help commands pass in the isolated target.

### Deterministic tone checkpoint

All are mandatory unless explicitly described as diagnostic:

1. the exact reviewed executable hash is recorded;
2. password-authenticated stock HPSS reaches Active mode 4;
3. the client sends exactly 500 probe packets with a recorded SSRC and extended
   sequence range;
4. authenticated, replay-accepted SRTCP names that SSRC, reports a highest
   sequence inside that range, and reports cumulative loss below packets sent;
5. the user explicitly reports hearing the deterministic tone on the Mac;
6. video and control remain active and application teardown is clean;
7. stock AVConference logs show no authentication, packet-parse, decoder,
   receiver, playback, speaker, or output-device failure.

Positive receiver/playback health counters strengthen gate 7 and are recorded
when emitted.  Their absence is neutral because unified-log visibility and
health cadence are not part of the media protocol.  The user's audible result
is the mandatory native-output observation.  Frequency-meter callback absence
is also neutral.

If gates 2-7 do not all pass, the production guards remain closed.  A rerun is
allowed only for a documented harness/environment failure, never to average
away a negative audible or protocol result.

### Windows microphone checkpoint

After the tone checkpoint is positive, all must pass in a fresh reviewed build:

1. the default Windows input device opens only after Active mode 4;
2. spoken audio is intelligible through the Mac's native output;
3. authenticated in-range SRTCP continues for the microphone sent range;
4. capture backlog and end-to-end latency remain bounded;
5. video/control remain responsive;
6. removing or failing the capture device degrades only P5 and releases capture;
7. normal exit and reconnect cleanly release and rebuild all generation-bound
   P5 resources.

Only this checkpoint permits documentation and CLI help to call P5 implemented.

## Failure Handling and Rollback

- Negotiation, key, direction, generation, codec, or transport error before
  capture: reject P5 before opening the device.
- Capture, conversion, encode, queue, packetization, SRTP, or send error:
  transition once to `Degraded`, release P5 resources, and keep the session.
- Missing authenticated in-range SRTCP by the bounded deadline: degrade P5 for
  the generation.
- Negative or unreported audible result: keep both production guards closed.
- Any evidence-helper sanitization failure: discard the run and do not publish
  raw artifacts.
- Rollback restores the two pre-network/pre-media production guards while
  retaining independently valid transport hardening and negative evidence.

## Expected File Scope

- `ard_re/run_live_hpss.py` and its tests: corrected bounded AVConference
  playback diagnostics and clean teardown evidence.
- `src/vnc/audio_input.rs`: explicit probe-to-microphone phase and budgets.
- `src/vnc/audio_io.rs`: capture lifecycle/health only if current behavior does
  not satisfy the verified interface.
- `src/vnc/audio_codec.rs`: non-silent round-trip guard and only evidence-backed
  codec corrections.
- `src/vnc/media_transport.rs`: typed sent-range/SRTCP evidence only if a proven
  integration gap remains.
- `src/vnc/hpss_viewer.rs`: reviewed probe cutover followed by capture
  coordination and isolated degradation.
- `src/main.rs`: final opt-in selection only after the tone checkpoint.
- `AGENTS.md`, `docs/ARD_PROTOCOL.md`, `docs/ARD_SESSION_PROTOCOL.md`, and
  `ard_re/P4_UDP_EVIDENCE.md`: update only from reviewed automated/live evidence.

## Completion Definition

P5 is complete only when a reviewed FreeRemoteDesk build, using the Windows
default microphone and only the existing username/password HPSS session, emits
intelligible spoken audio through the unmodified Mac's native output while
video/control remain active and teardown is clean.  No smaller transport,
encoding, logging, or unit-test result may substitute for that end state.
