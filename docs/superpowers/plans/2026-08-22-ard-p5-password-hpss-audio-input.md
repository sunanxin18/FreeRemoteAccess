# ARD P5 Password-Authenticated HPSS Audio Input Implementation Plan

> **STATUS: STOPPED / SUPERSEDED (2026-08-23).**  Stock framework recovery proved
> that Audio Chat requires IDS or an Apple-ID invitation address. The permitted
> username/password HPSS path has no recovered P5 branch. See
> `ard_re/P5_PROTOCOL_ANALYSIS.md`; do not execute the remaining tasks.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add opt-in PC-to-Mac microphone audio through the stock password-authenticated Apple HPSS mode-4 path, but only promote the microphone path after a bounded deterministic probe proves authenticated server consumption and native Mac audio output.

**Architecture:** Keep the existing RFB/HPSS control plane and generation-bound UDP media transport. Move outbound-audio sent-range and authenticated SRTCP confirmation ownership into `media_transport.rs`; put the deterministic probe scheduler and P5 runtime phase in a focused `audio_input.rs`; let `hpss_viewer.rs` only coordinate state, encoding, and non-fatal degradation. The implementation has a mandatory live checkpoint: a negative probe leaves both production guards closed and cancels the microphone cutover.

**Tech Stack:** Rust 2021, `anyhow`, existing `cpal`, `fdk-aac`, AES/HMAC SRTP/SRTCP implementation, Clap CLI, Python live-test helper, stock macOS unified logging over the existing credential-safe helper.

**Spec:** `docs/superpowers/specs/2026-08-22-ard-p5-password-hpss-audio-input-design.md`

## Global Constraints

- Never add, install, deploy, or run a Mac companion, daemon, launch agent, driver/plugin, relay, proxy, or test-only service.
- Use only the Mac username/password already used by the remote-login session. Do not request, store, or use Apple ID, iCloud, IDS, APNs, or QuickRelay credentials.
- Read secrets only through the ignored local provider used by `ard_re/run_live_hpss.py`; never place a host, username, password, or media key in source, documentation, command arguments, captures, or logs.
- Treat `D:\FreeRemoteDesk\target` as read-only and unhealthy. Every Cargo command in this plan must set `CARGO_TARGET_DIR=D:\FreeRemoteDesk\.superpowers\sdd\p5-build`; never run `cargo clean` against any target directory.
- This checkout has no `.git` metadata. Do not initialize Git and do not invent commit steps. Each worker reports the exact changed files and verification output; each reviewer audits the shared working tree before the next task starts.
- Follow TDD: write the named failing test, run it and record the expected failure, make the smallest implementation, rerun the focused test, then run the task gate.
- P5 is opt-in and generation-scoped. A P5-local failure closes only its encoder/capture/probe and leaves P3/P4, video, control, and clean teardown alive.
- No live run may be described as successful unless all applicable automated and live evidence gates below pass.

## File Structure

| File | Responsibility in this plan |
|---|---|
| `src/vnc/media_transport.rs` | Outbound extended RTP sequence metadata, sent-range accounting, authenticated RTCP classification, generation reset. |
| `src/vnc/audio_input.rs` | P5 phase machine and deterministic five-second probe scheduler; device-independent and unit-testable. |
| `src/vnc/mod.rs` | Feature-gated registration of `audio_input`. |
| `src/vnc/hpss_viewer.rs` | P5 orchestration, encoder/source lifecycle, diagnostics, isolated degradation. No raw RTCP parsing. |
| `src/vnc/audio_io.rs` | Default Windows input capture, bounded PCM queue, and surfaced stream/device errors. |
| `src/main.rs` | CLI selection of `AudioMediaFlow::PcToMac`; final user-facing opt-in text and fail-closed preconditions. |
| `ard_re/run_live_hpss.py` | Credential-safe bounded live runner that requires an explicitly supplied non-default-target executable and collects redacted native Mac evidence. |
| `docs/ARD_PROTOCOL.md` | Final verified P5 wire/runtime facts and remaining limits. |
| `docs/ARD_SESSION_PROTOCOL.md` | Final password-authenticated mode-4 session sequence and failure semantics. |
| `ard_re/P4_UDP_EVIDENCE.md` | Append-only live P5 evidence alongside, without rewriting, the prior negative observation. |
| `AGENTS.md` | Final roadmap state only after the corresponding live gate passes. |

---

### Task 1: Move outbound audio reception evidence into the media transport

**Files:**
- Modify: `src/vnc/media_transport.rs`
- Modify: `src/vnc/hpss_viewer.rs`

- [ ] **Step 1: Add failing sent-range and confirmation tests**

Add tests in `media_transport.rs` for all of these cases:

1. the first and last outbound packets expose monotonically increasing `extended_sequence` values;
2. a send crossing `0xffff -> 0x0000` increments the extended sequence instead of looking like a backwards range;
3. an authenticated report for another SSRC does not confirm;
4. a matching SSRC with `extended_highest_sequence` before or after the actual sent range does not confirm;
5. `cumulative_packets_lost >= packets_sent` does not confirm;
6. a matching, in-range report with `cumulative_packets_lost < packets_sent` confirms;
7. duplicate reports keep a single latched confirmation;
8. malformed, unauthenticated, replayed, and too-old SRTCP packets never confirm.

Use these public types as the target API:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboundRtpPacketMetadata {
    pub sequence: u16,
    pub extended_sequence: u32,
    pub timestamp: u32,
    pub ssrc: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboundAudioSentRange {
    pub ssrc: u32,
    pub first_extended_sequence: u32,
    pub last_extended_sequence: u32,
    pub packets_sent: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioReceptionEvidence {
    NotObserved,
    MatchingReport {
        extended_highest_sequence: u32,
        cumulative_packets_lost: i32,
    },
    Confirmed {
        extended_highest_sequence: u32,
        cumulative_packets_lost: i32,
    },
}
```

Run the RED slice:

```powershell
$env:CARGO_TARGET_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build'
cargo test media_transport::tests::outbound_audio_evidence -- --nocapture
```

Expected: compilation/test failure because the new types and behavior do not exist.

- [ ] **Step 2: Add exact extended-sequence metadata**

In `OutboundRtpStream::protect_audio_access_unit`, compute metadata from the rollover counter used to protect that packet:

```rust
let extended_sequence = self
    .rollover_counter
    .checked_mul(1 << u16::BITS)
    .and_then(|base| base.checked_add(u32::from(self.next_sequence)))
    .context("音频 RTCP 扩展序号溢出")?;
let metadata = OutboundRtpPacketMetadata {
    sequence: self.next_sequence,
    extended_sequence,
    timestamp: self.next_timestamp,
    ssrc: self.local_ssrc,
};
```

Do not infer an extended sequence by truncating the viewer's first/last `u16` values.

- [ ] **Step 3: Add a generation-bound evidence tracker to `MediaTransport`**

Add a private tracker with these invariants:

```rust
#[derive(Debug, Default)]
struct OutboundAudioEvidenceTracker {
    sent_range: Option<OutboundAudioSentRange>,
    evidence: AudioReceptionEvidence,
}
```

- `record_sent(metadata)` requires a stable SSRC, increments `packets_sent` with `checked_add`, and advances only `last_extended_sequence`.
- `observe_reports(reports)` ignores other SSRCs and latches `Confirmed` only when the report's extended highest sequence is inside the exact inclusive range and `i64::from(cumulative_packets_lost) < i64::from(packets_sent)`; do not use a narrowing cast.
- A matching but insufficient report may expose `MatchingReport` for diagnostics but must never overwrite `Confirmed`.
- Reset the tracker when a fresh `MediaTransport` generation is created/activated or the transport fails; never carry evidence across reconnects.
- `send_audio_access_unit` records the packet only after the UDP write succeeds.
- Expose read-only accessors `outbound_audio_sent_range()` and `audio_reception_evidence()`.

Parse reports only after `try_recv_decrypted` has authenticated and replay-checked SRTCP. For an accepted audio RTCP datagram, call `parse_rtcp_reception_reports` inside `media_transport.rs`; a parse error becomes `MediaDiscardReason::MalformedPacket` and is not forwarded as accepted data.

- [ ] **Step 4: Remove viewer-owned raw RTCP confirmation**

Delete from `ViewerMediaState`:

```rust
outbound_audio_ssrc
server_audio_reception_reports
first_sent_audio_sequence
last_sent_audio_sequence
server_confirmed_audio_reception
```

Delete `sequence_is_in_sent_range`, `reception_report_confirms_sent_range`, and the direct `parse_rtcp_reception_reports` import. The viewer may log the transport's typed `OutboundAudioSentRange` and `AudioReceptionEvidence`, but it must not parse RTCP or decide confirmation.

- [ ] **Step 5: Run the GREEN slice and task gate**

```powershell
$env:CARGO_TARGET_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build'
cargo test media_transport::tests -- --nocapture
cargo test hpss_viewer::tests -- --nocapture
cargo test --no-default-features media_transport::tests -- --nocapture
cargo fmt -- --check
```

Expected: all pass; existing Mac-to-PC reception and poll-fairness tests remain unchanged.

- [ ] **Step 6: Independent review checkpoint**

Reviewer verifies that confirmation can arise only from authenticated, replay-accepted SRTCP for the audio role and exact outbound SSRC/range; specifically inspect rollover, loss arithmetic, post-send accounting, and generation reset. Resolve all findings before Task 2.

---

### Task 2: Add a device-independent P5 phase machine and deterministic probe scheduler

**Files:**
- Create: `src/vnc/audio_input.rs`
- Modify: `src/vnc/mod.rs`

- [ ] **Step 1: Add failing phase and waveform tests**

Register the module under the same feature boundary as runtime viewer audio:

```rust
#[cfg(any(feature = "viewer", test))]
pub mod audio_input;
```

Add tests for:

- exact state order `Disabled -> Negotiating -> ReadyToSend -> AwaitingConfirmation -> Confirmed`;
- any P5-local error transitions once to terminal `Degraded`;
- generation mismatch/teardown returns to `Disabled` and clears all counters/deadlines;
- exactly 500 frames are offered, each exactly 480 mono samples;
- samples alternate `+10_000`/`-10_000` every 24 samples, yielding 1 kHz at 48 kHz;
- pacing never offers frames before their 10 ms due time and never offers more than the per-tick budget;
- an early typed confirmation is latched but does not shorten the 500-frame tone;
- after the last frame, lack of confirmation degrades at three seconds, not before;
- encode/send failure stops the probe without retrying indefinitely.

Run RED:

```powershell
$env:CARGO_TARGET_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build'
cargo test audio_input::tests -- --nocapture
```

- [ ] **Step 2: Implement explicit state and constants**

Use these exact protocol/probe constants and state shape:

```rust
pub const P5_PROBE_SAMPLE_RATE_HZ: u32 = 48_000;
pub const P5_PROBE_SAMPLES_PER_FRAME: usize = 480;
pub const P5_PROBE_FRAME_COUNT: u16 = 500;
pub const P5_PROBE_AMPLITUDE: i16 = 10_000;
pub const P5_PROBE_HALF_PERIOD_SAMPLES: usize = 24;
pub const P5_PROBE_FRAME_INTERVAL: Duration = Duration::from_millis(10);
pub const P5_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AudioInputPhase {
    Disabled,
    Negotiating { generation: u64 },
    ReadyToSend { generation: u64 },
    AwaitingConfirmation { generation: u64 },
    Confirmed { generation: u64 },
    Degraded { generation: u64, reason: String },
}
```

Implement the tone without floating-point drift:

```rust
pub fn p5_probe_pcm_frame() -> [i16; P5_PROBE_SAMPLES_PER_FRAME] {
    std::array::from_fn(|index| {
        if (index / P5_PROBE_HALF_PERIOD_SAMPLES) % 2 == 0 {
            P5_PROBE_AMPLITUDE
        } else {
            -P5_PROBE_AMPLITUDE
        }
    })
}
```

- [ ] **Step 3: Implement deterministic scheduling**

`AudioInputRuntime::start_probe(generation, now)` records `started_at`, `next_frame_at`, zero frames sent, and no final deadline. `take_due_probe_frames(generation, now, budget)` returns at most `budget` identical frames and advances `next_frame_at` by exactly 10 ms per returned frame. It may catch up after a delayed reader tick, but never exceeds the caller's budget or 500 total frames.

After frame 500 is successfully sent, set `confirmation_deadline = now + 3s`. `observe_transport_evidence(Confirmed { .. })` latches confirmation immediately but does not stop the remaining probe frames. Only after all 500 frames have been sent may the runtime expose `Confirmed`. If the deadline expires first, transition to `Degraded` with one stable Chinese reason.

- [ ] **Step 4: Run GREEN and task gate**

```powershell
$env:CARGO_TARGET_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build'
cargo test audio_input::tests -- --nocapture
cargo test --no-default-features audio_input::tests -- --nocapture
cargo fmt -- --check
```

- [ ] **Step 5: Independent review checkpoint**

Reviewer checks exact sample math, 500-frame bound, three-second deadline origin, early-confirmation behavior, generation reset, and starvation budget. Resolve all findings before Task 3.

---

### Task 3: Wire the bounded probe through the opt-in HPSS mode-4 path

**Files:**
- Modify: `src/main.rs`
- Modify: `src/vnc/hpss_viewer.rs`
- Modify: `src/vnc/audio_input.rs`

- [ ] **Step 1: Replace the old fail-closed tests with failing opt-in selection tests**

In `main.rs`, replace `hpss_audio_input_fails_closed_before_opening_a_session` with assertions that:

```rust
assert_eq!(hpssview_audio_flow(false).unwrap(), AudioMediaFlow::MacToPc);
assert_eq!(hpssview_audio_flow(true).unwrap(), AudioMediaFlow::PcToMac);
```

Keep the Clap `requires = "udp_media"` test and add a CLI test proving `--udp-audio-input` without `--udp-media` is rejected before credentials/network activity.

In `hpss_viewer.rs`, replace `viewer_api_rejects_remote_microphone_flow` with a test that accepts both typed flows but does not open a capture device during validation/negotiation.

Run RED:

```powershell
$env:CARGO_TARGET_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build'
cargo test hpss_audio_input_selects_remote_microphone_mode -- --nocapture
cargo test viewer_api_accepts_evidence_probe_without_device_side_effects -- --nocapture
```

- [ ] **Step 2: Open only the mode-4 negotiation guard**

Implement:

```rust
fn hpssview_audio_flow(udp_audio_input: bool) -> Result<AudioMediaFlow> {
    Ok(if udp_audio_input {
        AudioMediaFlow::PcToMac
    } else {
        AudioMediaFlow::MacToPc
    })
}
```

`validate_hpss_audio_flow` must accept `PcToMac`; it must not open CPAL, allocate an encoder, or send before the captured mode-4 Message 2 has been validated and `MediaTransportPhase::Active` is reached.

Update the CLI description to say this is an experimental, bounded, confirmation-gated PC-to-Mac probe. Do not call it microphone support yet.

- [ ] **Step 3: Replace capture-first startup with probe-first startup**

For this temporary evidence slice only:

- `ViewerMediaState` owns `AudioInputRuntime`, `Option<AacEldEncoder>`, and no `AudioCapture`.
- After the validated Message 2 activates a `PcToMac` transport, create the mono encoder, transition to `ReadyToSend`, and start the bounded probe.
- On every reader tick, call `take_due_probe_frames(..., MAX_AUDIO_ACCESS_UNITS_PER_READER_TICK)`, encode each returned frame through `AacEldEncoder::encode_pcm_frame`, then call `MediaTransport::send_audio_access_unit`.
- After draining UDP media, feed `transport.audio_reception_evidence()` into the runtime.
- A P5 encode/send/timeout failure calls one `degrade_audio_input(reason)` that drops the encoder and probe state; it must not write the viewer-wide fatal `error_slot`.
- Teardown and generation change drop the encoder and reset `AudioInputRuntime`.
- Log only phase, counts, extended sequence range, SSRC, and typed confirmation; never log keys, credentials, host, or raw payload.

Add a device-free integration test using the existing loopback media transport. It must send all 500 frames, inject a protected in-range Receiver Report, reach `Confirmed`, and prove the video/control loop remains serviceable. Add parallel negative tests for timeout and malformed/out-of-range reports that reach only P5 `Degraded`.

- [ ] **Step 4: Run the automated pre-live matrix**

```powershell
$env:CARGO_TARGET_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build'
cargo fmt -- --check
cargo test
cargo test --no-default-features
cargo build
cargo build --no-default-features
cargo run -- --help
cargo run -- hpssview --help
```

Expected: all commands pass from the fresh target directory. The final two help commands must not print or request credentials.

- [ ] **Step 5: Independent review checkpoint**

Reviewer verifies that this temporary executable can only run the bounded deterministic tone, opens no microphone, keeps the flag opt-in, waits for an active validated mode-4 transport, isolates P5 failure, and has no Apple ID/server-companion path. Resolve all findings before the live checkpoint.

---

### Task 4: Execute the mandatory sanitized live probe checkpoint

**Files:**
- Modify: `ard_re/run_live_hpss.py`
- Create runtime artifacts only under: `.superpowers/sdd/p5-live-probe/`

- [ ] **Step 1: Make the helper reject the default target and implicit executables**

Replace the hard-coded `target/debug/freeremotedesk.exe` with an explicit environment input:

```python
def _approved_executable() -> Path:
    raw = os.environ.get("FRD_EXECUTABLE")
    if not raw:
        raise RuntimeError("FRD_EXECUTABLE must name the reviewed P5 probe executable")
    executable = Path(raw).resolve(strict=True)
    forbidden = (ROOT / "target").resolve()
    if executable == forbidden or forbidden in executable.parents:
        raise RuntimeError("FRD_EXECUTABLE must not use the repository default target")
    if executable.name.lower() != "freeremotedesk.exe":
        raise RuntimeError("FRD_EXECUTABLE must name freeremotedesk.exe")
    return executable
```

Print only the executable SHA-256 and selected bounded mode, never the resolved host/login or environment. Add Python unit tests (or a small import-level test module under `ard_re/`) for missing path, forbidden default target, wrong filename, and accepted fresh target.

For `hpssview-audio-input`, do not start the existing delayed remote `/usr/bin/say` worker: that Mac-local sound would contaminate the audibility and output-frequency evidence. Keep that worker only for the Mac-to-PC `hpssview-audio` mode.

Require `FRD_LIVE_ARTIFACT_DIR` to resolve under `ROOT / ".superpowers" / "sdd"`; write only already-redacted stdout, stderr, executable SHA-256, selected mode, duration, and exit status there. Reject artifact paths outside that root.

- [ ] **Step 2: Add a native Mac output-frequency evidence mode**

Add a built-in-only remote log query whose predicate includes the exact recovered callback text:

```text
process == "ScreensharingAgent" AND
eventMessage CONTAINS[c] "updateOutputFrequencyLevel" AND
eventMessage CONTAINS[c] "data length"
```

The helper must redact host/user/password/IP/media keys before writing output. Do not use Frida, `sample`, a downloaded agent, `sudo`, or any third-party instrumentation for this gate.

- [ ] **Step 3: Build the reviewed probe executable in the fresh target**

```powershell
$env:CARGO_TARGET_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build'
cargo build --release
$env:FRD_EXECUTABLE = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build\release\freeremotedesk.exe'
$env:FRD_LIVE_MODE = 'hpssview-audio-input'
$env:FRD_LIVE_SECONDS = '15'
$env:FRD_LIVE_ARTIFACT_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-live-probe'
python ard_re/run_live_hpss.py
```

Pass the mode and bounded duration through the parent environment, not through credentials or target values on the command line. Preserve sanitized stdout/stderr and the executable hash under `.superpowers/sdd/p5-live-probe/`.

- [ ] **Step 4: Evaluate all six live gates**

Record PASS/FAIL for each item, with timestamps and sanitized evidence:

1. password-authenticated HPSS reaches active mode 4;
2. exactly the bounded outbound probe produces a recorded SSRC and extended sequence range;
3. authenticated SRTCP names that SSRC, reports an extended highest sequence inside that range, and reports cumulative loss below packets sent;
4. stock Mac unified logging contains a non-empty `updateOutputFrequencyLevel ... data length N` callback where `N > 0`;
5. the user audibly hears the deterministic tone on the Mac;
6. video/control remain active and teardown is clean.

UDP arrival, receive-queue growth, a report for another SSRC/range, or local encoding is not a substitute for any gate.

- [ ] **Step 5: Enforce the decision boundary**

- If all six gates pass: mark the checkpoint positive and continue to Task 5.
- If any of gates 3, 4, or 5 fails: stop the implementation sequence. Restore/retain the two production guards so `--udp-audio-input` fails closed before network/capture, keep the probe evidence as a negative result, do not execute Tasks 5-6, and introduce no fallback.
- A rerun is allowed only for a clearly documented test-harness failure, not to average away a protocol-negative result.

- [ ] **Step 6: Independent evidence review**

Reviewer checks that the executable hash matches the reviewed build, artifacts are sanitized, no prohibited Mac process was installed/run, the SRTCP report block matches the actual outbound SSRC/range, and the audible result came from the user. Resolve evidence ambiguities as FAIL, not inferred success.

---

### Task 5: Promote the proven path from deterministic probe to Windows microphone

**Precondition:** Task 4 has a reviewer-approved PASS for all six live gates. If not, this task is prohibited.

**Files:**
- Modify: `src/vnc/audio_io.rs`
- Modify: `src/vnc/audio_input.rs`
- Modify: `src/vnc/hpss_viewer.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add failing capture-health and production-phase tests**

Add device-independent tests proving:

- mono input remains mono and stereo input is averaged with `i32` intermediate arithmetic into exactly 480 samples;
- the 500 ms capture queue drops oldest whole frames, never grows without bound;
- a callback/device error is latched once and returned by the next `try_take_pc_to_mac_protocol_frame` call;
- consuming the latched error clears it so diagnostics are emitted once;
- capture open/read, conversion, encode, or send failure transitions P5 to `Degraded` while a simulated video/control tick still succeeds;
- production `--udp-audio-input` opens capture only after active mode-4 Message 2 validation;
- teardown/generation reset drops `AudioCapture`, encoder, sent range, and confirmation state.

Run RED:

```powershell
$env:CARGO_TARGET_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build'
cargo test audio_io::tests::capture_error -- --nocapture
cargo test hpss_viewer::tests::microphone -- --nocapture
```

- [ ] **Step 2: Surface CPAL input-stream errors to the owner**

Add a shared error latch:

```rust
#[derive(Default)]
struct AudioCaptureHealth {
    pending_error: Option<String>,
}

pub struct AudioCapture {
    _stream: Stream,
    buffer: Arc<Mutex<PcmCaptureBuffer>>,
    health: Arc<Mutex<AudioCaptureHealth>>,
    device_description: String,
}
```

Pass `health.clone()` into every `build_input_stream` callback and replace the current `eprintln!` error closure with a first-error latch. `try_take_pc_to_mac_protocol_frame` checks and consumes the error before reading PCM. Never panic from a CPAL callback or hold both health and PCM locks together.

- [ ] **Step 3: Replace the temporary probe source with real capture**

After mode-4 activation:

```rust
let encoder = AacEldEncoder::new_for_pc_to_mac()
    .context("创建 PC→Mac AAC-ELD 编码器失败")?;
let capture = AudioCapture::open_default()
    .context("打开 PC→Mac 音频输入设备失败")?;
```

Store them only in the P5 runtime. Each reader tick consumes at most `MAX_AUDIO_ACCESS_UNITS_PER_READER_TICK`, encodes exact mono frames, sends through `MediaTransport`, and observes typed transport evidence. Do not retain the deterministic tone as an automatic fallback; keep waveform/scheduler helpers only under `#[cfg(test)]` or an explicitly documented evidence-test boundary.

The production phase is:

```text
Disabled -> Negotiating -> ReadyToSend -> AwaitingConfirmation -> Confirmed
                                            \-> Degraded
```

Treat the first 500 successfully sent microphone access units as the production confirmation observation window. An early in-range authenticated report is latched; after packet 500, allow three more seconds for a report. If no valid report has been latched by that deadline, degrade P5 for the generation and drop capture. Confirmation does not relax queue bounds or error handling.

- [ ] **Step 4: Restore final CLI wording and tests**

Describe `--udp-audio-input` as experimental PC-to-Mac microphone audio over the password-authenticated stock HPSS path, still requiring `--udp-media`. Add tests that the disabled/default path opens no input device and sends no mode-4 offer.

- [ ] **Step 5: Run GREEN and the complete automated matrix**

```powershell
$env:CARGO_TARGET_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build'
cargo fmt -- --check
cargo test
cargo test --no-default-features
cargo build
cargo build --release
cargo build --no-default-features
cargo run -- --help
cargo run -- hpssview --help
```

- [ ] **Step 6: Independent review checkpoint**

Reviewer checks capture error propagation, lock ordering, bounded queue/reader budget, device lifetime, exact 480-sample contract, confirmation timeout, generation teardown, default-off behavior, and absence of a probe/server/identity fallback. Resolve all findings before Task 6.

---

### Task 6: Run live microphone acceptance and publish only verified status

**Files:**
- Modify after positive live acceptance only: `docs/ARD_PROTOCOL.md`
- Modify after positive live acceptance only: `docs/ARD_SESSION_PROTOCOL.md`
- Modify: `ard_re/P4_UDP_EVIDENCE.md`
- Modify after positive live acceptance only: `AGENTS.md`
- Create runtime artifacts only under: `.superpowers/sdd/p5-live-microphone/`

- [ ] **Step 1: Build and hash the final candidate in the fresh target**

```powershell
$env:CARGO_TARGET_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build'
cargo build --release
$env:FRD_EXECUTABLE = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build\release\freeremotedesk.exe'
$env:FRD_LIVE_MODE = 'hpssview-audio-input'
$env:FRD_LIVE_SECONDS = '15'
$env:FRD_LIVE_ARTIFACT_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-live-microphone'
python ard_re/run_live_hpss.py
```

Use the bounded `hpssview-audio-input` helper mode. The user speaks a short test phrase into the Windows default input. Do not put the phrase, credentials, target, or keys in artifacts.

- [ ] **Step 2: Evaluate microphone acceptance**

Require all of:

1. intelligible live audio emitted on the Mac;
2. continuing authenticated, in-range SRTCP for the actual outbound SSRC/range;
3. bounded capture backlog and no reader starvation;
4. video/control remain interactive;
5. disconnect tears down capture/socket/encoder cleanly;
6. a second bounded run with input-device removal degrades only P5 and preserves video/control/teardown.

Any missing item means P5 is not complete. Do not change roadmap status to complete.

- [ ] **Step 3: Update evidence without rewriting history**

Append to `ard_re/P4_UDP_EVIDENCE.md`:

- the prior receive-queue negative result remains under its original date;
- the exact final executable hash and tested macOS build are recorded without target identifiers;
- each probe/microphone gate is labeled `Verified`, `Candidate`, or `Blocked`;
- sanitized artifact paths and reviewer decision are recorded;
- no claim extends beyond the tested stock macOS build.

If microphone acceptance is negative, record the negative result there and leave the existing fail-closed status in the other docs.

- [ ] **Step 4: Update protocol and roadmap docs only for proven facts**

On a positive acceptance only:

- `docs/ARD_PROTOCOL.md`: document mode 4, direction, mono AAC-ELD/RFC3640/SRTP flow, typed SRTCP gate, and limits.
- `docs/ARD_SESSION_PROTOCOL.md`: document password-authenticated session sequence and P5-local degradation.
- `AGENTS.md`: change P5 from pending to opt-in experimental implementation with the exact tested-build boundary; retain the client-only and no-Apple-ID rules.

Do not state that arbitrary macOS versions, long rollover, high loss, or multiple devices are proven.

- [ ] **Step 5: Final verification-before-completion gate**

Run from a fresh process with the safe target variable set:

```powershell
$env:CARGO_TARGET_DIR = 'D:\FreeRemoteDesk\.superpowers\sdd\p5-build'
cargo fmt -- --check
cargo test
cargo test --no-default-features
cargo build
cargo build --release
cargo build --no-default-features
cargo run -- --help
cargo run -- hpssview --help
rg -n -i "apple.?id|icloud|quickrelay|apns|companion|daemon|launch agent|driver|proxy" src docs AGENTS.md ard_re/P4_UDP_EVIDENCE.md
```

Audit every search hit for prohibited implementation or inaccurate prose. Also verify no file under `D:\FreeRemoteDesk\target` was created, modified, moved, or deleted during this plan.

- [ ] **Step 6: Final independent acceptance review**

Reviewer compares the working tree and all evidence against every binding constraint and acceptance gate in the approved spec. Completion requires zero unresolved correctness/security/rule findings and a positive microphone live gate. Otherwise report P5 as blocked or partially implemented with the exact failed gate.

## Execution Order and Stop Rules

1. Execute Tasks 1-3 sequentially with one implementation worker and one independent review after each task.
2. Task 4 is a hard live checkpoint, not documentation theater. A protocol-negative result stops the plan and preserves fail-closed behavior.
3. Execute Task 5 only after Task 4 is fully positive and reviewed.
4. Execute Task 6 only after Task 5's full automated matrix is green.
5. Never trade away client-only, password-only, default-off, credential-redaction, or P5 failure-isolation rules to make a test pass.

## Spec Coverage Self-Review

- Client-only/no companion: Global Constraints; Tasks 3-6 reviews.
- Username/password only; no Apple ID: Global Constraints; final audit.
- Mode-4 validated control plane: Tasks 1 and 3.
- Exact AAC-ELD/RFC3640/SRTP path: Tasks 2, 3, and 5.
- Typed authenticated SRTCP confirmation: Task 1.
- Exact five-second/500-frame/1-kHz probe and three-second wait: Task 2.
- Mandatory Mac native callback plus audibility: Task 4.
- Real microphone, queue bound, and device-removal isolation: Tasks 5-6.
- Generation reset and no retry loop: Tasks 1-3 and 5.
- Negative-result fail-closed decision: Task 4 stop rule.
- Evidence/doc accuracy and no overclaim: Task 6.

No placeholder, inferred wire field, server-side fallback, or identity-based fallback is part of this plan.
