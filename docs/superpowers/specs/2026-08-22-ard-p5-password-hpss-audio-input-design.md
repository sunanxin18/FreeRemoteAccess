# ARD P5 Password-Authenticated HPSS Audio Input Design

**Status:** Superseded by `ard_re/P5_PROTOCOL_ANALYSIS.md`; implementation forbidden
**Date:** 2026-08-22
**Scope:** P5 PC-to-Mac audio only

> Later stock framework recovery proved that Apple Audio Chat requires IDS or
> an Apple-ID invitation address. The username/password-only project boundary
> excludes that path. Use `ard_re/P5_PROTOCOL_ANALYSIS.md` as the authoritative
> disposition and do not resume this implementation plan.

## Objective

Add opt-in PC-to-Mac audio to the existing password-authenticated HPSS session.
FreeRemoteDesk captures the Windows default input device, encodes Apple-compatible
AAC-ELD, protects it as SRTP, and sends it through the audio socket negotiated by
the stock Mac Screen Sharing service.

Success means the unmodified Mac consumes authenticated packets and emits the
audio through its native Screen Sharing/AVConference path. UDP delivery alone,
a successful local encode, or a syntactically valid Message 2 is not success.

## Binding Product Constraints

- FreeRemoteDesk remains a remote-login client. No custom application, daemon,
  launch agent, driver/plugin, relay, proxy, or test fallback may be installed,
  deployed, or run as part of the solution on the Mac.
- Authentication uses only the Mac username and password already accepted by
  the remote-login/screen-sharing session.
- The Windows client must not request, retain, or use Apple ID, iCloud, IDS,
  APNs, or QuickRelay identity credentials.
- The Mac's stock `screensharingd`, `ScreensharingAgent`, `avconferenced`, and
  their existing private frameworks may be used without modification.
- P5 stays opt-in through `--udp-audio-input`; it never becomes a silent default.
- P3, P4, video, control, and session teardown must continue if P5 degrades.
- If the stock password-authenticated service does not consume mode-4 audio,
  P5 remains fail-closed. There is no server-side or identity-based fallback.
- Credentials and media keys never appear in source, docs, command-line
  arguments, captures, or unsanitized test output.

## Evidence Baseline

### Verified static evidence

- `AVCMediaStreamNegotiator` mode 4 is the mono `RemoteMicrophone` offer; mode 8
  is the Mac-to-PC Screen Sharing audio offer.
- `ScreensharingAgent` creates one `AVCAudioStream` inside `SSUDPSender`, applies
  cipher suite 5, assigns separate send and receive media keys, and accepts a
  non-default negotiated audio direction.
- `-[SSUDPSender start]` starts `audioStream` after starting the video streams.
- `SSUDPSender` implements audio-specific callbacks for RTP timeout, received
  RTCP packets, input/output frequency levels, stream start, and stream stop.
- The existing Windows implementation already owns mode-4 offer generation,
  mono AAC-ELD encoding, RFC 3640 single-AU bundling, outbound SRTP/SRTCP,
  microphone capture, and sent-sequence accounting. Two explicit guards keep
  this dormant before session/network activation.

### Verified negative live evidence

- A prior bounded mode-4 run negotiated direction 2 and sent PC-to-Mac packets.
- Packets reached the connected Mac UDP socket but accumulated in its receive
  queue. No authenticated receive proof or audio output proof was observed.
- An SRTCP report whose highest sequence is outside the actual sent range is not
  proof of reception.

### Candidate interpretation

The negative run may reflect a configuration, endpoint, payload, timing, source,
or AVConference activation mismatch. Class naming alone does not prove that the
stock stream is send-only because the same object is configured with a receive
key and receive-side callbacks. This design therefore permits one evidence-gated
mode-4 investigation before declaring the password path unavailable.

## Architecture

The existing RFB/HPSS session remains the sole control plane. P5 changes only the
audio role selected while constructing the version-3 client MediaStream
configuration:

```text
Mac account authentication / encrypted HPSS session
    -> Message 1 media-port announcement
    -> mode-4 RemoteMicrophone offer in version-3 client 0x1c configuration
    -> Apple Message 2 answer, validated before activation
    -> generation-bound connected audio UDP socket
    -> Windows PCM -> mono AAC-ELD -> RFC 3640 -> RTP -> SRTP
    -> stock ScreensharingAgent AVCAudioStream
    -> authenticated SRTCP reception evidence
```

The P5 sender is a leaf of the existing media transport. It does not own session
authentication, socket negotiation, key derivation, or the network-reader loop.

## Components and Ownership

### `media_negotiation.rs`

- Owns the named `RemoteMicrophone` mode and mode-4 offer.
- Validates the captured mode-4 answer container and its direction/audio
  contract before the transport becomes active.
- Keeps directional key material typed as viewer-to-server and server-to-viewer.

### `srtp.rs` and `media_transport.rs`

- Own outbound RTP sequence, timestamp, SSRC, rollover counter, SRTP protection,
  SRTCP scheduling, connected-source checks, and sent-range accounting.
- Use viewer-to-server material for outbound audio and server-to-viewer material
  only for inbound/control traffic.
- Expose typed confirmation outcomes rather than interpreting arbitrary RTCP in
  the viewer.

### `audio_codec.rs`

- Own the Apple-compatible mono 48 kHz AAC-ELD encoder contract and RFC 3640
  single-AU payload.
- Accept exactly one protocol frame per encode call and never invent a codec
  profile from adjacent binary strings.

### `audio_io.rs`

- Own Windows default-input capture and conversion into exact mono protocol
  frames.
- Bounds the capture queue and drops old local frames rather than increasing
  end-to-end latency without limit.

### `hpss_viewer.rs`

- Owns the P5 runtime phase, degradation isolation, user diagnostics, and the
  scheduling of capture/encode/send work.
- Does not parse crypto or synthesize transport confirmation independently.

### `main.rs`

- Owns CLI fail-closed selection.
- `--udp-audio-input` still requires `--udp-media` and selects P5 only after all
  pre-network validation succeeds.

## Runtime State Machine

```text
Disabled
  --explicit flag--> Negotiating
  --valid Message 2 / mode 4--> ReadyToSend
  --first protected packet--> AwaitingConfirmation
  --authenticated in-range SRTCP--> Confirmed

Negotiating / ReadyToSend / AwaitingConfirmation / Confirmed
  --P5-local failure--> Degraded

Any state
  --session generation change or teardown--> Disabled
```

- `Disabled` opens no capture device and sends no PC-to-Mac audio.
- `Negotiating` has no device side effects; invalid or mismatched mode-4 answers
  fail closed.
- `ReadyToSend` opens the device/encoder only after the media transport is
  active.
- `AwaitingConfirmation` sends a bounded deterministic probe before normal
  microphone audio is considered interoperable.
- `Confirmed` means an authenticated SRTCP reception report references a
  sequence inside the actual sent range. It does not by itself prove audible
  playback; live acceptance adds the Mac output evidence below.
- `Degraded` is terminal for the current generation. It closes capture and
  encoder resources, logs once, and does not retry per packet.

## Probe and Production Data Flow

### Evidence probe

The first live implementation slice uses a deterministic, bounded mono tone:
a 1 kHz square wave with signed-16 amplitude 10,000, encoded as 500 consecutive
480-sample frames (five seconds at 48 kHz).
It exercises the production encoder, bundler, RTP/SRTP sender, socket, and
control-report parser. It does not open the Windows microphone and is not kept
as an automatic fallback.

The probe sends at most those 500 access units and waits at most three seconds
after the last packet for confirmation. An early confirmation is latched but
does not truncate the five-second audible tone. The probe stops sooner only on
session generation change, transport teardown, or device-independent
encode/send error. It never loops indefinitely.

### Microphone flow

After the live probe gate is satisfied, the opt-in production path opens the
Windows default input device. Exact protocol frames are encoded and sent under
the existing per-reader-tick budget. Capture backlog is bounded. A slow network
or encoder cannot starve video/control processing.

## Confirmation and Acceptance Gates

### Automated gate

All of the following must pass before a live probe:

- mode 4 and mode 8 remain distinct and byte-valid;
- a captured Apple mode-4 answer is accepted and mismatched answers fail closed;
- mono AAC-ELD AudioSpecificConfig, frame length, channel count, and RFC 3640
  header match verified fixtures;
- outbound audio uses viewer-to-server keys and a nonzero SSRC;
- sequence/timestamp/rollover and SRTCP sent-range matching are deterministic;
- duplicate, malformed, unauthenticated, replayed, and out-of-range reports do
  not confirm reception;
- P5 timeout/degradation does not stop P3/P4, video, control, or teardown;
- both default and no-default-feature test/build configurations remain green.

### Live probe gate

A bounded run against the authorized stock Mac must show all of:

1. password-authenticated HPSS reaches active mode-4 media;
2. the client sends authenticated SRTP packets with recorded sequence range;
3. the Mac returns authenticated SRTCP whose report block names the outbound
   audio SSRC, whose extended highest sequence falls in the actual sent range,
   and whose cumulative loss is lower than the number of packets sent;
4. built-in Mac diagnostics show a non-empty audio output-frequency callback,
   without installing an instrumentation agent;
5. the deterministic tone is audibly emitted on the Mac;
6. video/control remain active and clean teardown succeeds.

If gates 3-5 do not all pass, the probe result is negative and the production
guard remains. UDP arrival, receive-queue growth, or an unrelated RTCP report is
insufficient.

### Microphone acceptance gate

After the probe passes, repeat a bounded run using the Windows default input and
spoken audio. Confirm intelligible Mac playback, continued in-range SRTCP, bounded
latency/backlog, and isolated degradation on capture-device removal.

## Error Handling

- Invalid mode-4 answer, wrong direction, missing key, or generation mismatch:
  fail the P5 activation before capture starts.
- Capture open/read, PCM conversion, encode, packetization, SRTP, or send error:
  transition P5 once to `Degraded`; keep the rest of the session active.
- No in-range SRTCP before the bounded deadline: stop P5 for the generation and
  report that server consumption is unconfirmed.
- RTP/RTCP noise follows the existing authenticated discard/replay policy and
  consumes the audio role's fair poll budget.
- Reconnect or exact generation commit creates fresh sockets, keys, encoder,
  capture state, sequence state, and confirmation state.

## Security and Privacy

- Username/password continue through ignored local credential providers and
  environment variables, never process arguments.
- Apple ID and related identity services are structurally absent from P5.
- Media keys remain generation-bound and redacted from all diagnostics.
- Audio capture exists only while the user explicitly enables P5 and the current
  generation is active. Degradation and teardown drop it immediately.
- Probe audio is deterministic and contains no captured speech.
- Live reports contain counts, phases, and redacted addresses only.

## Documentation and Evidence Updates

On implementation, update `AGENTS.md`, `docs/ARD_PROTOCOL.md`,
`docs/ARD_SESSION_PROTOCOL.md`, and `ard_re/P4_UDP_EVIDENCE.md` without rewriting
historical negative evidence. Record new facts as `Verified`, remaining reverse
inferences as `Candidate`, and unavailable behavior as `Blocked`.

## Non-Goals

- Apple ID/IDS/QuickRelay/APNs emulation.
- A Mac companion, driver, plugin, daemon, relay, or proxy.
- Changing macOS audio routing outside the stock Screen Sharing behavior.
- Enabling P5 by default.
- Claiming arbitrary-loss, long-rollover, or multi-version interoperability from
  one bounded run.
- Expanding P1, P2, P3, P4, or unrelated runtime cleanup in the P5 task set.

## Decision on Negative Results

If the correctly negotiated, correctly keyed, bounded mode-4 probe still lacks
both in-range authenticated SRTCP and Mac audio-output evidence, route 1 is
considered disproved for the tested stock macOS build. The two P5 guards remain,
the feature remains visibly blocked, and no prohibited fallback is introduced.
