# ARD Media Receive Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make P3 Mac→PC audio and P4 UDP reception survive authenticated reordering, hostile datagrams, per-role backlog, and audio-only failures without terminating screen/control.

**Architecture:** `srtp.rs` remains the cryptographic/replay owner, `media_transport.rs` turns one socket read into a typed accepted/discarded/empty outcome and owns a fair per-role poll round, while `audio_codec.rs` classifies RTP sequence direction before decoding. `hpss.rs` and `hpss_viewer.rs` consume the same poll API; audio failures enter an explicit degraded state instead of escaping into the viewer reader loop.

**Tech Stack:** Rust 2021, UDP sockets, SRTP/SRTCP, AAC-ELD via `fdk-aac`, `anyhow`, Rust unit tests and loopback integration tests.

**Spec:** `docs/superpowers/specs/2026-08-22-ard-p3-p4-p6-hardening.md`

## Global Constraints

- P5 remains fail-closed; do not enable `--udp-audio-input` or send `RemoteMicrophone` mode 4 to HPSS.
- Production wire values, lengths, flags, masks, budgets, and timeouts use semantic symbols owned by one module.
- Network noise may be discarded; socket/state/generation contract failures remain fatal.
- Every active media role gets its own `MAX_MEDIA_DATAGRAMS_PER_ROLE_PER_POLL` budget; discarded datagrams consume that role's budget.
- Existing verified AAC-ELD, SRTP/SRTCP, Message 1/2, P1, and P2 behavior must not regress.
- This workspace has no Git metadata. Do not initialize Git; each task ends with a named verification checkpoint instead of a commit.

---

### Task 1: Classify authenticated late audio without touching decoder state

**Files:**
- Modify: `src/vnc/audio_codec.rs:374-437`
- Test: `src/vnc/audio_codec.rs:581-632`

**Interfaces:**
- Consumes: `parse_rtp_packet`, `ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT`, the existing AAC-ELD decoder.
- Produces: `AudioReceiveOutcome`, returned by `ArdAudioReceiver::decode_rtp_packet(&mut self, packet: &[u8]) -> Result<AudioReceiveOutcome>`.

- [ ] **Step 1: Add a failing four-packet sequence test**

Add this independent behavior test beside the existing audio receiver tests:

```rust
#[test]
fn audio_receiver_discards_late_packet_without_advancing_forward_state() {
    const FIRST_SEQUENCE: u16 = 100;
    const FIRST_TIMESTAMP: u32 = 48_000;
    let timestamp = |advance: u32| {
        FIRST_TIMESTAMP + advance * ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT as u32
    };
    let mut receiver = ArdAudioReceiver::new().unwrap();

    assert!(matches!(
        receiver.decode_rtp_packet(&audio_rtp(
            FIRST_SEQUENCE,
            timestamp(0),
            ARD_AUDIO_RTP_PAYLOAD_TYPE,
        )).unwrap(),
        AudioReceiveOutcome::Decoded(_)
    ));

    let AudioReceiveOutcome::Decoded(gapped) = receiver.decode_rtp_packet(&audio_rtp(
        FIRST_SEQUENCE + 2,
        timestamp(2),
        ARD_AUDIO_RTP_PAYLOAD_TYPE,
    )).unwrap() else { panic!("forward packet must decode") };
    assert_eq!(gapped.concealed_access_units, 1);

    assert_eq!(
        receiver.decode_rtp_packet(&audio_rtp(
            FIRST_SEQUENCE + 1,
            timestamp(1),
            ARD_AUDIO_RTP_PAYLOAD_TYPE,
        )).unwrap(),
        AudioReceiveOutcome::DiscardedLate {
            sequence: FIRST_SEQUENCE + 1,
            last_forward_sequence: FIRST_SEQUENCE + 2,
        }
    );

    let AudioReceiveOutcome::Decoded(next) = receiver.decode_rtp_packet(&audio_rtp(
        FIRST_SEQUENCE + 3,
        timestamp(3),
        ARD_AUDIO_RTP_PAYLOAD_TYPE,
    )).unwrap() else { panic!("next forward packet must decode") };
    assert_eq!(next.concealed_access_units, 0);
}
```

- [ ] **Step 2: Run the RED test**

Run:

```powershell
cargo test audio_receiver_discards_late_packet_without_advancing_forward_state
```

Expected: compilation fails because `AudioReceiveOutcome` does not exist, proving the test exercises the new contract.

- [ ] **Step 3: Add the typed outcome and half-space classifier**

Add beside `DecodedAudioPacket`:

```rust
const RTP_SEQUENCE_FORWARD_HALF_SPACE: u16 = 1 << (u16::BITS - 1);

#[derive(Debug, Eq, PartialEq)]
pub enum AudioReceiveOutcome {
    Decoded(DecodedAudioPacket),
    DiscardedLate {
        sequence: u16,
        last_forward_sequence: u16,
    },
}

fn forward_sequence_advance(last: u16, candidate: u16) -> Option<u16> {
    let advance = candidate.wrapping_sub(last);
    (advance != 0 && advance < RTP_SEQUENCE_FORWARD_HALF_SPACE).then_some(advance)
}
```

In `decode_rtp_packet`, return `DiscardedLate` before timestamp validation or AAC decode when `forward_sequence_advance` is `None`. For forward packets, keep the current bounded concealment and timestamp alignment checks. Update `last_sequence`/`last_timestamp` only after successful AAC decode, and wrap the success in `AudioReceiveOutcome::Decoded`.

- [ ] **Step 4: Update existing tests and run GREEN**

Change existing success destructuring to require `AudioReceiveOutcome::Decoded`. Keep payload-type rejection as `Err`.

Run:

```powershell
cargo test audio_receiver_
cargo test apple_aac_eld_
```

Expected: all selected tests pass; the new test proves `100 -> 102 -> 101 -> 103` has one concealment and one late discard.

- [ ] **Step 5: Record checkpoint**

Record `src/vnc/audio_codec.rs` and the exact two test commands in the implementation log. Do not initialize a repository.

---

### Task 2: Convert UDP reads into accepted/discarded/empty outcomes

**Files:**
- Modify: `src/vnc/media_transport.rs:1-180,578-620,665-900`
- Modify: `src/vnc/srtp.rs:17-112,329-510`
- Test: `src/vnc/media_transport.rs:665-900`

**Interfaces:**
- Consumes: `SrtpReceiver::open`, `open_srtcp_packet`, negotiated `MediaRole` and remote address.
- Produces: `MediaReceiveOutcome`, `MediaDiscardReason`, `MediaDiscardCounters`, `SrtcpReceiver`, and `MediaTransport::try_recv_decrypted(...) -> Result<MediaReceiveOutcome>`.

- [ ] **Step 1: Write loopback RED tests for each discard class**

Extend the existing `udp_transport_binds_before_activation_and_preserves_role_and_generation` setup so it retains the legitimate remote socket and local audio address. Add assertions equivalent to:

```rust
let attacker = UdpSocket::bind((REMOTE_MEDIA_ADDRESS, 0)).unwrap();
attacker.send_to(b"wrong-source", local).unwrap();
assert_eq!(
    transport.try_recv_decrypted(7, MediaRole::Audio).unwrap(),
    MediaReceiveOutcome::Discarded(MediaDiscardReason::UnexpectedSource)
);

remote.send_to(&[], local).unwrap();
assert_eq!(
    transport.try_recv_decrypted(7, MediaRole::Audio).unwrap(),
    MediaReceiveOutcome::Discarded(MediaDiscardReason::EmptyDatagram)
);

remote.send_to(&[0x80], local).unwrap();
assert_eq!(
    transport.try_recv_decrypted(7, MediaRole::Audio).unwrap(),
    MediaReceiveOutcome::Discarded(MediaDiscardReason::TruncatedHeader)
);

let mut bad_tag = protected.clone();
*bad_tag.last_mut().unwrap() ^= 1;
remote.send_to(&bad_tag, local).unwrap();
assert_eq!(
    transport.try_recv_decrypted(7, MediaRole::Audio).unwrap(),
    MediaReceiveOutcome::Discarded(MediaDiscardReason::AuthenticationFailed)
);

remote.send_to(&protected, local).unwrap();
assert_eq!(
    transport.try_recv_decrypted(7, MediaRole::Audio).unwrap(),
    MediaReceiveOutcome::Accepted(MediaDatagram::Rtp(plaintext.clone()))
);
remote.send_to(&protected, local).unwrap();
assert_eq!(
    transport.try_recv_decrypted(7, MediaRole::Audio).unwrap(),
    MediaReceiveOutcome::Discarded(MediaDiscardReason::ReplayOrTooOld)
);
assert_eq!(
    transport.try_recv_decrypted(7, MediaRole::Audio).unwrap(),
    MediaReceiveOutcome::Empty
);
```

Use separate sequence numbers for the authentication-failure and accepted packets so the RED test does not accidentally depend on replay state.
Protect one legitimate SRTCP packet with `SrtcpSender`, send it twice, and assert the first is accepted while the second is `ReplayOrTooOld`.

- [ ] **Step 2: Run the RED test**

Run:

```powershell
cargo test udp_transport_discards_untrusted_datagrams_and_accepts_the_next_valid_packet
```

Expected: compilation fails on the new outcome/reason types.

- [ ] **Step 3: Implement semantic outcomes and counters**

Add:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaDiscardReason {
    UnexpectedSource,
    EmptyDatagram,
    TruncatedHeader,
    MalformedPacket,
    AuthenticationFailed,
    ReplayOrTooOld,
}

#[derive(Debug, Eq, PartialEq)]
pub enum MediaReceiveOutcome {
    Empty,
    Accepted(MediaDatagram),
    Discarded(MediaDiscardReason),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MediaDiscardCounters {
    pub unexpected_source: u64,
    pub empty_datagram: u64,
    pub truncated_header: u64,
    pub malformed_packet: u64,
    pub authentication_failed: u64,
    pub replay_or_too_old: u64,
}
```

Store counters in `MediaTransport`, increment with `saturating_add`, and expose a read-only `discard_counters(&self)`. Keep phase/generation/role/inbound-state checks outside the untrusted-packet classification so they remain `Err`. Map non-`WouldBlock` socket errors to `Err`. Classify an empty datagram before indexing; classify a packet shorter than the RTP/RTCP discriminator as `TruncatedHeader`; classify structurally invalid RTP/RTCP as `MalformedPacket`; classify a failed HMAC as `AuthenticationFailed`; map authenticated replay-window rejection to `ReplayOrTooOld`. Preserve the underlying error only for sampled diagnostics, never as a session-fatal result.

Replace the stateless SRTCP open path with:

```rust
pub struct SrtcpReceiver {
    keys: SrtpSessionKeys,
    highest_index: Option<u32>,
    replay_window: u64,
}

impl SrtcpReceiver {
    pub fn open(&mut self, packet: &[u8]) -> Result<Option<Vec<u8>>> {
        // verify HMAC, parse the encrypted 31-bit index, reject replay/too-old,
        // decrypt/validate RTCP, then advance the replay window
    }
}
```

Use a named 64-packet SRTCP replay window over the authenticated 31-bit index. Authentication happens before replay classification; state advances only after successful decrypt and RTCP validation. `InboundCryptoStream` owns both `SrtpReceiver` and `SrtcpReceiver`.

Add a private `record_discard` helper that logs only counts 1, 2, 4, 8, ... using `count.is_power_of_two()`.

- [ ] **Step 4: Run GREEN and preserve rollover/replay coverage**

Run:

```powershell
cargo test udp_transport_
cargo test srtp_receiver_
cargo test outbound_audio_rtp_advances_rollover_counter_at_sequence_wrap
```

Expected: all pass; generation mismatch remains an error and all five network discard classes leave the transport active.

- [ ] **Step 5: Record checkpoint**

Record the two modified modules, discard counter snapshot from the test, and commands.

---

### Task 3: Put fair per-role draining in the transport owner

**Files:**
- Modify: `src/vnc/media_transport.rs:28-55,515-620`
- Modify: `src/vnc/hpss.rs:86-92,680-694`
- Modify: `src/vnc/hpss_viewer.rs:38-41,247-265`
- Test: `src/vnc/media_transport.rs:665-900`

**Interfaces:**
- Consumes: `MediaReceiveOutcome` from Task 2.
- Produces: `MAX_MEDIA_DATAGRAMS_PER_ROLE_PER_POLL`, `MediaPollSummary`, and `MediaTransport::drain_receive_round`.

- [ ] **Step 1: Add a RED test for budget accounting and role rotation**

Use loopback sockets for Audio, VideoStream1, and VideoStream2 in a transport configured by a Message 1 fixture. Queue more than the test budget for Audio and one valid packet for each video role. Because the production budget is large, add a private `drain_receive_round_with_budget` used by the public wrapper and tests.

```rust
let first = transport
    .drain_receive_round_with_budget(7, 2, |role, _| {
        accepted.push(role);
        Ok(())
    })
    .unwrap();
assert_eq!(first.per_role(MediaRole::Audio).processed, 2);
assert_eq!(first.per_role(MediaRole::VideoStream1).accepted, 1);
assert_eq!(first.per_role(MediaRole::VideoStream2).accepted, 1);

let second = transport
    .drain_receive_round_with_budget(7, 2, |_, _| Ok(()))
    .unwrap();
assert_ne!(second.role_order[0], first.role_order[0]);
```

Also queue two discarded packets for one role and assert they consume its two-packet budget.

- [ ] **Step 2: Run the RED test**

Run:

```powershell
cargo test udp_poll_round_preserves_per_role_fairness_and_rotates_start
```

Expected: compilation fails because transport-owned draining and summaries do not exist.

- [ ] **Step 3: Implement one owner for role order and quota**

Add:

```rust
pub const MAX_MEDIA_DATAGRAMS_PER_ROLE_PER_POLL: usize = 256;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MediaRolePollStats {
    pub processed: usize,
    pub accepted: usize,
    pub discarded: usize,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct MediaPollSummary {
    pub accepted_total: usize,
    pub discarded_total: usize,
    pub role_order: Vec<MediaRole>,
    pub roles: Vec<(MediaRole, MediaRolePollStats)>,
}
```

Implement `MediaPollSummary::per_role(&self, role: MediaRole) -> MediaRolePollStats`, returning the recorded stats or the default zero value. Store `next_poll_role_index: usize` in `MediaTransport`. `drain_receive_round_with_budget` rotates the active role vector by that index, advances the index once per round, performs at most `budget` reads per role, breaks only that role on `Empty`, calls the handler only for `Accepted`, and counts `Discarded` as processed. The public `drain_receive_round` supplies `MAX_MEDIA_DATAGRAMS_PER_ROLE_PER_POLL`.

- [ ] **Step 4: Replace both duplicated loops**

In `hpss.rs`:

```rust
let summary = media_transport.drain_receive_round(0, |role, datagram| {
    record_authenticated_datagram(session, role, datagram)
})?;
Ok(summary.accepted_total)
```

In `hpss_viewer.rs`:

```rust
let summary = transport.drain_receive_round(0, |role, datagram| {
    media_state.accept(role, datagram).map(|_| ())
})?;
Ok(summary.accepted_total)
```

Remove both local 256 constants.

- [ ] **Step 5: Run GREEN**

Run:

```powershell
cargo test udp_poll_round_
cargo test receive_one_udp_round
cargo test --no-default-features
```

Expected: fairness test passes, headless tests pass, and no-default build path does not depend on viewer types.

- [ ] **Step 6: Record checkpoint**

Record the single quota owner and both removed duplicate symbols.

---

### Task 4: Degrade only Mac→PC audio on codec or device failure

**Files:**
- Modify: `src/vnc/hpss_viewer.rs:43-230`
- Modify: `src/vnc/audio_codec.rs:374-437`
- Test: `src/vnc/hpss_viewer.rs:1918-end`

**Interfaces:**
- Consumes: `AudioReceiveOutcome` and `MediaReceiveOutcome` from Tasks 1-2.
- Produces: `AudioOutputPhase`, `MediaAcceptOutcome`, and a testable audio-result transition.

- [ ] **Step 1: Add RED tests for late discard and audio degradation**

Add a small pure state wrapper and test it through `ViewerMediaState` without opening a real device:

```rust
#[test]
fn audio_failure_degrades_audio_without_returning_a_viewer_fatal_error() {
    let mut state = ViewerMediaState::new(AudioMediaFlow::MacToPc).unwrap();
    let malformed_authenticated_rtp = vec![0u8; 3];

    let outcome = state
        .accept(MediaRole::Audio, MediaDatagram::Rtp(malformed_authenticated_rtp))
        .unwrap();

    assert!(matches!(outcome, MediaAcceptOutcome::AudioDegraded));
    assert!(matches!(state.audio_output_phase(), AudioOutputPhase::Degraded { .. }));
    assert_eq!(state.authenticated_video_packets, 0);
}
```

Add a test feeding an `AudioReceiveOutcome::DiscardedLate` through an extracted `accept_audio_outcome` helper; assert it increments `late_audio_packets` and returns `MediaAcceptOutcome::Discarded` without degrading audio.

- [ ] **Step 2: Run RED tests**

Run:

```powershell
cargo test audio_failure_degrades_audio_without_returning_a_viewer_fatal_error
cargo test late_audio_packet_is_counted_without_degrading_output
```

Expected: compilation fails on the new state/outcome APIs.

- [ ] **Step 3: Implement explicit audio phase and bounded diagnostic**

Add:

```rust
#[derive(Debug, Eq, PartialEq)]
enum AudioOutputPhase {
    ReadyToStart,
    Active,
    Degraded { reason: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MediaAcceptOutcome {
    Applied,
    Discarded,
    AudioDegraded,
}
```

Keep `Option<AudioPlayback>` as the concrete device owner. On RTP parse/AAC decode, `AudioPlayback::open_default`, or `enqueue_interleaved_stereo` failure, call one `degrade_audio(error)` method that drops playback, replaces `ArdAudioReceiver` only when a new generation starts, stores a single reason, logs once, and returns `Ok(MediaAcceptOutcome::AudioDegraded)`. Do not swallow RTCP parser failures that represent internal authenticated-control contract violations.

On `AudioReceiveOutcome::DiscardedLate`, increment a saturating `late_audio_packets` counter and return `Discarded`. On decoded audio, keep existing non-silent/concealment counters and return `Applied`.

- [ ] **Step 4: Reset degraded audio only at generation replacement**

Add `ViewerMediaState::reset_generation(&mut self) -> Result<()>` that creates a fresh `ArdAudioReceiver`, clears playback and the phase to `ReadyToStart`, and preserves lifetime diagnostics. Call it from the same atomic P1 generation commit that resets MVS state; do not retry devices on every packet.

- [ ] **Step 5: Run GREEN and viewer regression tests**

Run:

```powershell
cargo test audio_failure_degrades_
cargo test late_audio_packet_
cargo test dynamic_resolution_
cargo test --all-features
```

Expected: audio failures are non-fatal, late packets are counted, and generation transition tests still pass.

- [ ] **Step 6: Record checkpoint**

Record the exact error classes that degrade audio and the classes that remain viewer-fatal.

---

### Task 5: Verify the complete media reliability slice

**Files:**
- Modify only if verification exposes a regression in Tasks 1-4.

**Interfaces:**
- Consumes: all interfaces in this plan.
- Produces: a clean local verification record ready for plan-level review.

- [ ] **Step 1: Run formatting and focused tests**

```powershell
cargo fmt -- --check
cargo test audio_receiver_
cargo test udp_transport_
cargo test udp_poll_round_
cargo test audio_failure_degrades_
```

Expected: all commands exit 0.

- [ ] **Step 2: Run both feature matrices**

```powershell
cargo test
cargo test --no-default-features
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
```

Expected: all commands exit 0; no warning is suppressed merely to pass the gate.

- [ ] **Step 3: Review evidence boundaries**

Confirm with source search:

```powershell
rg -n "udp_audio_input|RemoteMicrophone|PcToMac" src docs
rg -n "MAX_UDP_DATAGRAMS_PER_READER_TICK|MAX_MEDIA_DATAGRAMS_PER_ROLE_PER_POLL" src
```

Expected: HPSS audio input remains fail-closed; the old viewer quota symbol is absent; the transport symbol is the only production quota owner.

- [ ] **Step 4: Record plan checkpoint**

Record test totals and the final discard/audio counters proven by tests. Do not claim live loss/reordering proof until the separate live gate is run.
