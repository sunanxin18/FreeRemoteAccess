# ARD P5 Native Playback and Windows Microphone Implementation Plan

> **STATUS: STOPPED / SUPERSEDED (2026-08-23).**  Do not execute the remaining
> tasks. Offline recovery of stock `ScreenSharing.framework` proved that Apple
> Audio Chat requires IDS or an Apple-ID invitation address; the permitted
> username/password HPSS path has no recovered branch. See
> `ard_re/P5_PROTOCOL_ANALYSIS.md`. `--udp-audio-input` must remain fail-closed.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove that the existing password-authenticated HPSS PC-to-Mac audio path reaches native Mac playback with a reviewed 500-frame deterministic tone, then replace that probe with bounded Windows microphone capture and complete an intelligible-audio live acceptance run without changing the Mac.

**Architecture:** Keep SRTP/SRTCP and generation ownership in `media_transport.rs`, keep the device-independent P5 state machine in `audio_input.rs`, keep AAC-ELD contracts in `audio_codec.rs`, and expose Windows capture through a typed source interface in `audio_io.rs`. `hpss_viewer.rs` coordinates one explicitly selected source mode with bounded per-tick work. A positive, user-audible tone checkpoint is a hard prerequisite for the separately reviewed microphone cutover.

**Tech Stack:** Rust 2021, Clap, CPAL, FDK AAC, existing HPSS/RTP/SRTP stack, Python `unittest`, stock macOS unified logging through the existing credential-backed live helper.

**Spec:** `docs/superpowers/specs/2026-08-23-ard-p5-native-playback-and-microphone-design.md`

## Global Constraints

- [ ] Do not add, install, copy, or run any companion application, daemon, launch agent, driver, plugin, relay, proxy, fallback, or injected code on the Mac.
- [ ] Use only the existing username/password encrypted remote-login flow. Do not use or retain Apple ID, iCloud, IDS, APNs, or QuickRelay credentials.
- [ ] Read test secrets only through the ignored local credential provider. Never place host addresses, usernames, passwords, media keys, executable paths, or raw unredacted remote output in source, documentation, commands, test names, captures, or review ledgers.
- [ ] Do not use Frida, `sample`, `sudo`, downloaded agents, or non-stock instrumentation on the Mac. Stock non-privileged `/usr/bin/log show` is allowed.
- [ ] Keep `--udp-audio-input` explicit and dependent on `--udp-media`. Invalid combinations must fail before address parsing, credentials, session construction, capture, or networking.
- [ ] P5 errors must degrade and release P5 only. Video, control, P3, P4, normal close, and reconnect remain usable.
- [ ] Do not initialize Git. Make source/document edits only with `apply_patch`.
- [ ] Treat `D:\FreeRemoteDesk\target` as read-only. Every Cargo command uses:

  ```powershell
  $env:CARGO_TARGET_DIR='C:\Users\rabbit\AppData\Local\Temp\freeremotedesk-p5-20260822'
  $env:CARGO_INCREMENTAL='0'
  $env:CARGO_PROFILE_DEV_DEBUG='0'
  $env:CARGO_PROFILE_TEST_DEBUG='0'
  ```

- [ ] Maintain the task ledger under `.superpowers/sdd/2026-08-23-ard-p5-native-playback-and-microphone/`. For each task, record the exact reviewed file SHA-256 values, commands, exit codes, reviewer result, and sanitized artifact locations with `apply_patch`; do not copy source snapshots or secrets into the ledger.
- [ ] Before each live run, independently review the exact source state and hash the exact executable. A build or source change invalidates the earlier review and requires a new hash.
- [ ] Frequency-meter callbacks are diagnostic-only. Their absence never fails a gate and never substitutes for user-reported native audibility.

## Task 1: Correct the bounded AVConference playback evidence mode

**Files:**

- Modify: `ard_re/run_live_hpss.py`
- Modify: `ard_re/test_run_live_hpss_hardening.py`
- Record: `.superpowers/sdd/2026-08-23-ard-p5-native-playback-and-microphone/task-1-review.md`

- [ ] **Step 1: Add failing predicate and dispatch tests**

  Add these tests to `LiveHpssHardeningTests`:

  ```python
  def test_avconference_playback_mode_uses_bounded_health_aware_predicate(self):
      command = run_live_hpss._remote_command_for_mode(
          "remote-avconference-playback-log"
      )
      for required in (
          "avconferenced", "ScreensharingAgent", "AVCAudioStream",
          "VCAudioReceiver", "VCAudioPlayer", "speakerProcsCalled",
          "playback", "audioIO", "output device", "Failed", "RTP", "SRTP",
          "timeout",
      ):
          self.assertIn(required, command)
      self.assertNotIn('NOT eventMessage CONTAINS[c] "Health"', command)

  def test_avconference_playback_mode_is_allowed_and_routes_as_remote_only(self):
      self.assertIn(
          "remote-avconference-playback-log", run_live_hpss.ALLOWED_MODES
      )
      self.assertEqual(
          run_live_hpss._remote_command_for_mode(
              "remote-avconference-playback-log"
          ),
          run_live_hpss.REMOTE_AVCONFERENCE_PLAYBACK_LOG_COMMAND,
      )
  ```

- [ ] **Step 2: Run the focused RED test**

  Run:

  ```powershell
  python -m unittest ard_re.test_run_live_hpss_hardening.LiveHpssHardeningTests.test_avconference_playback_mode_uses_bounded_health_aware_predicate ard_re.test_run_live_hpss_hardening.LiveHpssHardeningTests.test_avconference_playback_mode_is_allowed_and_routes_as_remote_only
  ```

  Expected RED: the new mode/constant is absent.

- [ ] **Step 3: Add the exact bounded mode**

  Add `REMOTE_AVCONFERENCE_PLAYBACK_LOG_COMMAND` and `remote-avconference-playback-log` to `ALLOWED_MODES`, `_remote_command_for_mode`, and the remote-only branch in `main`. Its predicate must select only `avconferenced` or `ScreensharingAgent` events containing at least one of:

  ```text
  AVCAudioStream, VCAudioReceiver, VCAudioPlayer, speakerProcsCalled,
  playback, audioIO, output device, Failed, RTP, SRTP, timeout
  ```

  Do not add a blanket `NOT ... "Health"` clause. Keep the existing redaction and artifact-directory enforcement unchanged.

- [ ] **Step 4: Run GREEN and the full helper suite**

  Run:

  ```powershell
  python -m unittest ard_re.test_run_live_hpss_hardening.LiveHpssHardeningTests.test_avconference_playback_mode_uses_bounded_health_aware_predicate ard_re.test_run_live_hpss_hardening.LiveHpssHardeningTests.test_avconference_playback_mode_is_allowed_and_routes_as_remote_only
  python -m unittest ard_re.test_run_live_hpss_hardening
  ```

  Expected GREEN: both focused tests and the full helper suite pass; existing redaction, worker isolation, partial-output, terminate, and kill tests remain green.

- [ ] **Step 5: Review and ledger checkpoint**

  Independently inspect the predicate for boundedness, remote-only dispatch, and absence of credential/address leakage. Run `Get-FileHash -Algorithm SHA256` on both files and record hashes, commands, exit codes, and review result in `task-1-review.md` with `apply_patch`.

## Task 2: Prove mono AAC-ELD output and prepare the reviewed tone artifact

**Files:**

- Modify: `src/vnc/audio_codec.rs`
- Modify: `src/vnc/audio_input.rs`
- Modify: `src/vnc/hpss_viewer.rs`
- Modify: `src/main.rs`
- Record: `.superpowers/sdd/2026-08-23-ard-p5-native-playback-and-microphone/task-2-review.md`

- [ ] **Step 1: Add the failing mono decoder round-trip test**

  Add `pc_to_mac_probe_roundtrip_is_non_silent_and_mono` in `audio_codec.rs`. The test must create `AacEldEncoder::new_for_pc_to_mac()`, encode `p5_probe_pcm_frame()`, decode it with the new mono constructor, assert exactly 480 mono samples, and assert at least one decoded sample has absolute amplitude greater than 256.

  Target constructor shape:

  ```rust
  impl AacEldDecoder {
      pub fn new_for_pc_to_mac() -> Result<Self> {
          Self::new_with_contract(
              &ARD_AAC_ELD_PC_TO_MAC_AUDIO_SPECIFIC_CONFIG,
              ARD_AUDIO_PC_TO_MAC_CHANNEL_COUNT,
          )
      }
  }
  ```

  Refactor `new()` through the same private constructor and store the expected output channel/sample count in `AacEldDecoder`; do not weaken the current Mac-to-PC stereo assertions.

- [ ] **Step 2: Run the codec RED test**

  Run:

  ```powershell
  cargo test pc_to_mac_probe_roundtrip_is_non_silent_and_mono
  ```

  Expected RED: `new_for_pc_to_mac` or the mono output contract is absent.

- [ ] **Step 3: Implement the mono decoder contract and make the test GREEN**

  Configure raw AAC-ELD with `ARD_AAC_ELD_PC_TO_MAC_AUDIO_SPECIFIC_CONFIG`, minimum/maximum output channels of one, and exact decoded sample count `ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT * 1`. Preserve sample-rate validation at 48 kHz.

  Run:

  ```powershell
  cargo test pc_to_mac_probe_roundtrip_is_non_silent_and_mono
  cargo test audio_codec --lib
  ```

- [ ] **Step 4: Add explicit source-mode and phase tests before changing guards**

  Add the compile-time-reviewed source distinction in `audio_input.rs`:

  ```rust
  #[derive(Clone, Copy, Debug, Eq, PartialEq)]
  pub enum AudioInputSourceMode {
      DeterministicProbe,
      WindowsMicrophone,
  }
  ```

  Expand `AudioInputPhase` without removing generation ownership:

  ```rust
  #[derive(Clone, Debug, Eq, PartialEq)]
  pub enum AudioInputPhase {
      Disabled,
      Negotiating { generation: u64 },
      ProbeReady { generation: u64 },
      ProbeSending { generation: u64 },
      ProbeAwaitingReport { generation: u64 },
      ProbeConfirmed { generation: u64 },
      MicrophoneReady { generation: u64 },
      MicrophoneStreaming { generation: u64 },
      Degraded { generation: u64, reason: String },
  }
  ```

  Change construction to `AudioInputRuntime::new(source_mode)`. `mark_transport_active` selects `ProbeReady` or `MicrophoneReady`; `start_probe` and `take_due_probe_frames` are valid only in probe phases; `mark_microphone_streaming` is valid only from `MicrophoneReady`. Preserve token-deduplicated `record_probe_frame_sent`; add `record_microphone_access_unit_sent(generation, now)` to count only successful microphone sends. Add or adapt focused tests named:

  ```text
  probe_sends_exactly_500_frames_before_awaiting_report
  early_in_range_confirmation_latches_until_frame_500
  wrong_generation_and_unconfirmed_evidence_never_confirm
  teardown_clears_source_mode_probe_and_confirmation_state
  microphone_phase_cannot_start_in_deterministic_probe_mode
  ```

  Preserve the typed `AudioReceptionEvidence` boundary and the three-second confirmation deadline. Do not treat `ProbeConfirmed` as audible success.

- [ ] **Step 5: Run the state-machine RED slice, implement, then run GREEN**

  Run each named test before implementation and confirm that the new phase/source expectations fail. Then implement the smallest state-machine change and run:

  ```powershell
  cargo test audio_input --lib
  ```

- [ ] **Step 6: Re-enable only the deterministic probe path with viewer tests**

  In this task's reviewed build, `--udp-audio-input --udp-media` selects `AudioMediaFlow::PcToMac` and `AudioInputSourceMode::DeterministicProbe`. Put the reviewed selection in one visible viewer-owned constant so the later cutover is a one-line, independently reviewable change:

  ```rust
  const REVIEWED_AUDIO_INPUT_SOURCE_MODE: AudioInputSourceMode =
      AudioInputSourceMode::DeterministicProbe;
  ```

  Update the CLI help to say that the flag runs a bounded 500-frame deterministic native-playback probe and does not open the microphone.

  Add/adapt these tests before removing either guard:

  ```text
  hpss_audio_input_selects_pc_to_mac_probe_before_session_selection
  viewer_api_allows_reviewed_deterministic_probe_flow
  deterministic_probe_never_constructs_audio_capture
  deterministic_probe_sends_at_most_32_access_units_per_reader_tick
  deterministic_probe_releases_encoder_after_confirm_or_degrade
  p5_degrade_keeps_video_and_control_serviceable
  ```

  `validate_hpss_audio_flow` must accept only the reviewed probe configuration. Do not add microphone construction to this task.

- [ ] **Step 7: Run focused and full automated gates**

  Run:

  ```powershell
  cargo test audio_input --lib
  cargo test audio_codec --lib
  cargo test builds_current_binary_plist_offers_with_mode_specific_protobuf --lib
  cargo test audio_flow_selects_the_named_system_audio_or_remote_microphone_mode --lib
  cargo test pc_to_mac_rfc3640_bundle_matches_apple_single_au_header --lib
  cargo test outbound_audio_evidence_confirms_only_the_exact_sent_range --lib
  cargo test outbound_audio_evidence_replayed_matching_report_does_not_confirm_after_latch_clear --lib
  cargo test hpss_audio_input --lib
  cargo test deterministic_probe --lib
  cargo test
  cargo test --no-default-features
  cargo build
  cargo build --no-default-features
  cargo run -- --help
  cargo run -- hpssview --help
  cargo fmt -- --check
  ```

  If a name filter matches zero tests, stop and correct the test command; a zero-test pass is not evidence.

- [ ] **Step 8: Build, review, and hash the exact tone executable**

  Run:

  ```powershell
  cargo build --release
  Get-FileHash -Algorithm SHA256 'C:\Users\rabbit\AppData\Local\Temp\freeremotedesk-p5-20260822\release\freeremotedesk.exe'
  ```

  Independently review the exact diff by comparing current file content and the ledger's prior hashes, confirm that capture is unreachable, and record the reviewed source hashes, executable hash, automated matrix, and reviewer result in `task-2-review.md` with `apply_patch`.

## Task 3: Execute the hard deterministic-tone checkpoint

**Artifacts:**

- Viewer: `.superpowers/sdd/p5-native-tone/`
- Playback log: `.superpowers/sdd/p5-native-tone-playback-log/`
- Gate record: `.superpowers/sdd/2026-08-23-ard-p5-native-playback-and-microphone/tone-gate.md`

- [ ] **Step 1: Verify preconditions without exposing secrets**

  Confirm that the exact release executable SHA-256 equals the reviewed Task 2 value, the Mac is reachable through the authorized local provider, and both artifact directories are new/empty. Do not print provider values.

- [ ] **Step 2: Run the bounded interactive tone viewer**

  Set `FRD_EXECUTABLE` to the reviewed isolated release executable, `FRD_LIVE_MODE=hpssview-audio-input`, `FRD_LIVE_SECONDS=30`, and `FRD_LIVE_ARTIFACT_DIR` to `.superpowers/sdd/p5-native-tone/`, then run:

  ```powershell
  python ard_re\run_live_hpss.py
  ```

  The user must explicitly report whether the tone is heard through the Mac's native output and must close the viewer normally before 30 seconds. A harness `terminate` or `kill`, a crash, silence, or an unreported audible result fails the gate.

- [ ] **Step 3: Collect the bounded playback diagnostic after the viewer closes**

  With the same reviewed executable, set `FRD_LIVE_MODE=remote-avconference-playback-log`, `FRD_LIVE_SECONDS=5`, and `FRD_LIVE_ARTIFACT_DIR` to `.superpowers/sdd/p5-native-tone-playback-log/`, then run the same helper. Inspect only redacted artifacts.

- [ ] **Step 4: Evaluate every mandatory tone gate**

  Record PASS/FAIL and evidence location for:

  1. exact reviewed executable hash;
  2. password-authenticated HPSS Active mode 4;
  3. exactly 500 sent packets with one recorded SSRC and extended sequence range;
  4. authenticated, replay-accepted SRTCP for the same SSRC with highest sequence inside the actual sent range and cumulative loss below packets sent;
  5. explicit user report that the tone was heard through native Mac output;
  6. responsive video/control and clean viewer exit (`reached_duration=false`, `termination_method=none`, `exit_status=0`);
  7. no stock AVConference authentication, parse, decode, receiver, playback, speaker, or output-device failure.

  Positive health counters are supporting evidence. Missing health counters and missing frequency callbacks are neutral.

- [ ] **Step 5: Apply the hard stop**

  If any item 2-7 fails or is unreported, write `tone-gate.md` as CLOSED and stop the plan. Do not edit the microphone guard, do not open capture, and do not reinterpret SRTCP reception as playback. A rerun is permitted only for a documented harness/environment failure.

  If all items pass, write `tone-gate.md` as OPEN with the executable hash, sanitized evidence paths, explicit audibility result, teardown state, and reviewer sign-off. Only then begin Task 4.

## Task 4: Add a typed, bounded Windows microphone source

**Hard prerequisite:** Task 3 `tone-gate.md` is OPEN for the exact reviewed tone artifact.

**Files:**

- Modify: `src/vnc/audio_io.rs`
- Modify: `src/vnc/audio_input.rs`
- Record: `.superpowers/sdd/2026-08-23-ard-p5-native-playback-and-microphone/task-4-review.md`

- [ ] **Step 1: Add failing capture health and source-interface tests**

  Introduce the public device-independent boundary:

  ```rust
  #[derive(Clone, Debug, Default, Eq, PartialEq)]
  pub struct AudioCaptureStatus {
      pub queued_frames: usize,
      pub dropped_frames: u64,
      pub terminal_error: Option<String>,
  }

  pub trait PcToMacAudioSource {
      fn description(&self) -> &str;
      fn try_take_protocol_frame(&mut self) -> Result<Option<Vec<i16>>>;
      fn status(&self) -> Result<AudioCaptureStatus>;
  }
  ```

  Add tests named:

  ```text
  capture_status_reports_queue_depth_and_dropped_oldest_frames
  capture_terminal_error_is_latched_as_stable_generic_health
  pc_to_mac_source_returns_exact_480_sample_mono_frames
  capture_queue_never_exceeds_500_milliseconds
  ```

  The tests use `PcmCaptureBuffer` and injected error state, not the ignored real-device test.

- [ ] **Step 2: Run the RED slice**

  Run:

  ```powershell
  cargo test capture_status --lib
  cargo test pc_to_mac_source --lib
  cargo test capture_queue_never_exceeds_500_milliseconds --lib
  ```

- [ ] **Step 3: Implement typed health without leaking device/error details**

  Make `AudioCapture` implement `PcToMacAudioSource`. Share a small health object between `AudioCapture` and the CPAL error callback. On callback failure, latch one stable generic terminal error such as `"默认音频输入流已停止"`; do not place device names or backend-specific error strings into diagnostics/artifacts. `status()` returns queue depth, monotonic dropped-frame count, and the latched generic error.

  Keep the current 500 ms maximum and drop-oldest policy. Update the stale doc comment that currently says the capture yields stereo frames; the PC-to-Mac public contract is 480 mono samples.

- [ ] **Step 4: Add state-machine microphone confirmation tests**

  Add tests in `audio_input.rs` named:

  ```text
  microphone_phase_requires_windows_microphone_source_selection
  microphone_first_500_sent_units_define_confirmation_window
  microphone_early_report_latches_until_500_successful_sends
  microphone_continues_streaming_after_confirmation
  microphone_confirmation_timeout_degrades_only_current_generation
  microphone_teardown_clears_sent_range_and_source_state
  ```

  The state machine counts successfully sent access units, not captured or encoded frames. It must enter `MicrophoneReady` only for `WindowsMicrophone`, then `MicrophoneStreaming` only after the source has opened. The first 500 successful sends share the existing typed SRTCP confirmation semantics.

- [ ] **Step 5: Implement and run GREEN**

  Run:

  ```powershell
  cargo test audio_io --lib
  cargo test audio_input --lib
  cargo fmt -- --check
  ```

- [ ] **Step 6: Review and ledger checkpoint**

  Review queue bounds, drop-oldest semantics, generic error sanitization, generation reset, and the absence of any device open in probe/negotiation phases. Record exact file hashes and results in `task-4-review.md` with `apply_patch`.

## Task 5: Integrate the microphone into the viewer and final CLI cutover

**Files:**

- Modify: `src/vnc/hpss_viewer.rs`
- Modify: `src/main.rs`
- Modify only if a proven typed-evidence gap exists: `src/vnc/media_transport.rs`
- Record: `.superpowers/sdd/2026-08-23-ard-p5-native-playback-and-microphone/task-5-review.md`

- [ ] **Step 1: Add a fake source and failing viewer lifecycle tests**

  Extend `ViewerMediaState` with:

  ```rust
  audio_input_source_mode: AudioInputSourceMode,
  audio_capture: Option<Box<dyn PcToMacAudioSource>>,
  ```

  Keep production construction behind a private closure seam so tests can prove open ordering without touching CPAL:

  ```rust
  fn open_audio_input_source_with<F>(&mut self, generation: u64, open: F) -> Result<()>
  where
      F: FnOnce() -> Result<Box<dyn PcToMacAudioSource>>;
  ```

  The production wrapper passes a closure that boxes `AudioCapture::open_default()`. Provide a test-only fake source/factory that exposes open count, exact PCM frames, queue status, terminal error, and drop count without using CPAL. Add tests named:

  ```text
  microphone_source_is_not_opened_before_active_mode4
  microphone_source_opens_once_after_active_mode4
  microphone_tick_sends_at_most_32_access_units
  microphone_empty_queue_does_not_busy_loop_or_degrade
  microphone_encode_send_or_terminal_capture_error_degrades_only_p5
  microphone_drop_count_is_reported_without_unbounded_logging
  microphone_confirmed_stream_keeps_capture_and_encoder_alive
  generation_change_drops_capture_encoder_queue_and_confirmation
  normal_teardown_drops_capture_before_reader_exit
  video_and_control_remain_serviceable_during_microphone_streaming
  ```

- [ ] **Step 2: Run the viewer RED slice**

  Run each named filter and verify it executes at least one failing test. Also run:

  ```powershell
  cargo test microphone_ --lib
  ```

- [ ] **Step 3: Implement bounded coordination**

  After the tone gate, change only `REVIEWED_AUDIO_INPUT_SOURCE_MODE` from `DeterministicProbe` to `WindowsMicrophone`, then independently review the resulting artifact. Open `AudioCapture::open_default()` only after the password-authenticated Message 2 direction is mode 4 and the current generation's transport is Active. Keep at most `MAX_AUDIO_ACCESS_UNITS_PER_READER_TICK` (32) capture/encode/send operations per reader tick.

  For each frame: take exactly 480 mono samples, encode with `AacEldEncoder::new_for_pc_to_mac()`, call `MediaTransport::send_audio_access_unit`, and record success in `AudioInputRuntime`. Empty capture queues are normal. Open/read/device-removal, conversion, encode, SRTP, or send errors transition once to `Degraded`, drop `audio_capture` and `audio_encoder`, and leave the reader loop/session alive.

  Continue streaming after authenticated in-range confirmation. Keep diagnostic logging rate-limited and do not print device names, addresses, credentials, keys, or raw remote data.

- [ ] **Step 4: Finalize CLI semantics only after viewer tests pass**

  Keep `hpssview_audio_flow(true) -> PcToMac`, retain Clap's `requires = "udp_media"`, and update help to describe live Windows-default-microphone streaming over the reviewed password-authenticated HPSS path. Keep `false -> MacToPc` unchanged.

  Add/adapt tests named:

  ```text
  hpss_audio_input_selects_pc_to_mac_before_session_selection
  udp_audio_input_without_udp_media_is_rejected_by_cli_before_dispatch
  default_hpssview_audio_flow_remains_mac_to_pc
  ```

- [ ] **Step 5: Run focused GREEN and regression slices**

  Run:

  ```powershell
  cargo test microphone_ --lib
  cargo test hpss_audio_input --lib
  cargo test udp_audio_input_without_udp_media --lib
  cargo test media_transport --lib
  cargo test hpss_viewer --lib
  cargo fmt -- --check
  ```

  Do not modify `media_transport.rs` unless a failing typed-evidence test proves a gap. If it is modified, add a failing regression for matching generation, SSRC, actual sent range, replay rejection, and out-of-range rejection before the change.

- [ ] **Step 6: Run the full matrix and review exact final build**

  Run:

  ```powershell
  cargo test
  cargo test --no-default-features
  cargo build
  cargo build --no-default-features
  cargo run -- --help
  cargo run -- hpssview --help
  cargo fmt -- --check
  cargo build --release
  Get-FileHash -Algorithm SHA256 'C:\Users\rabbit\AppData\Local\Temp\freeremotedesk-p5-20260822\release\freeremotedesk.exe'
  ```

  Independently review source-mode cutover, capture-open ordering, per-tick bound, generic health, P5-only degradation, teardown/reconnect ownership, and CLI preflight order. Record all source hashes, exact executable hash, commands, exit codes, and review result in `task-5-review.md` with `apply_patch`.

## Task 6: Execute the live Windows microphone acceptance

**Artifacts:**

- Viewer: `.superpowers/sdd/p5-live-microphone/`
- Playback log: `.superpowers/sdd/p5-live-microphone-playback-log/`
- Acceptance record: `.superpowers/sdd/2026-08-23-ard-p5-native-playback-and-microphone/microphone-gate.md`

- [ ] **Step 1: Verify exact final artifact and device preconditions**

  Verify the release executable hash equals Task 5's independently reviewed value. Confirm the Windows default microphone is available without printing its name and that the two new artifact directories are empty.

- [ ] **Step 2: Run the interactive microphone acceptance**

  Set `FRD_LIVE_MODE=hpssview-audio-input`, `FRD_LIVE_SECONDS=30`, `FRD_EXECUTABLE` to the reviewed final release executable, and `FRD_LIVE_ARTIFACT_DIR` to `.superpowers/sdd/p5-live-microphone/`. Run the helper, speak a distinctive short phrase, verify it is intelligible on the Mac native output, exercise pointer/keyboard/video responsiveness, and close normally before the bound.

- [ ] **Step 3: Collect playback diagnostics**

  Run `remote-avconference-playback-log` with `.superpowers/sdd/p5-live-microphone-playback-log/` after normal viewer close. Reject artifacts if redaction or helper isolation fails.

- [ ] **Step 4: Validate the positive microphone gate**

  Require all of:

  1. capture opens only after Active mode 4;
  2. the user's distinctive phrase is intelligible through Mac native output;
  3. authenticated, replay-accepted SRTCP names the same SSRC and confirms an extended highest sequence inside the first-500 successful-send range;
  4. queue depth/drop diagnostics remain bounded and observed latency is acceptable for conversation;
  5. video and control remain responsive;
  6. normal close is clean (`reached_duration=false`, `termination_method=none`, `exit_status=0`).

- [ ] **Step 5: Validate failure isolation and reconnect**

  In a separately recorded run, make the default input temporarily unavailable or disconnect it using normal Windows device controls. Confirm only P5 becomes `Degraded`, capture is released, video/control remain active, and the viewer can close normally. Restore the device, reconnect in a fresh session, and confirm capture/state are rebuilt and spoken audio is again intelligible. Do not perform destructive device/driver changes.

- [ ] **Step 6: Record acceptance or keep P5 pending**

  Write `microphone-gate.md` with exact executable hash, sanitized artifact paths, SRTCP range result, bounded queue result, explicit user audibility/intelligibility result, responsiveness, failure-isolation result, teardown/reconnect result, and reviewer sign-off.

  If any requirement is negative or unreported, keep P5 pending and record the exact failed gate. Do not describe transport reception or local capture as completed P5.

## Task 7: Documentation, final verification, and completion review

**Hard prerequisite:** Task 6 `microphone-gate.md` records every acceptance item as PASS.

**Files:**

- Modify: `AGENTS.md`
- Modify: `docs/ARD_PROTOCOL.md`
- Modify: `docs/ARD_SESSION_PROTOCOL.md`
- Modify: `ard_re/P4_UDP_EVIDENCE.md`
- Record: `.superpowers/sdd/2026-08-23-ard-p5-native-playback-and-microphone/final-review.md`

- [ ] **Step 1: Update claims to the exact reviewed evidence**

  Mark P5 implemented only after Task 6 passes. Separate verified native playback, authenticated reception evidence, capture lifecycle, and diagnostics. State that frequency callbacks are optional diagnostics and that positive user audibility was required. Do not copy credentials, target identifiers, executable paths, media keys, raw logs, or device names.

- [ ] **Step 2: Run stale-claim and secret scans**

  Run targeted scans for the invalidated gate and accidental sensitive values:

  ```powershell
  rg -n "updateOutputFrequencyLevel|P5.*(pending|blocked|禁用)|AppleID|Apple ID|iCloud|QuickRelay|vcMediaStreamSRTP.*Key" AGENTS.md docs ard_re\P4_UDP_EVIDENCE.md src
  ```

  Review every match manually. Do not place real secret values in the search command.

- [ ] **Step 3: Re-run the final verification matrix**

  Run:

  ```powershell
  python -m unittest ard_re.test_run_live_hpss_hardening
  cargo fmt -- --check
  cargo test
  cargo test --no-default-features
  cargo build
  cargo build --no-default-features
  cargo run -- --help
  cargo run -- hpssview --help
  ```

- [ ] **Step 4: Perform independent completion review**

  Review the implementation against every section of the approved spec and this plan. Verify the no-server-program/no-Apple-ID constraints, exact live artifact hashes, positive tone gate, positive microphone gate, P5-only failure isolation, clean teardown/reconnect, and truthful documentation. Record file hashes, commands, exit codes, live-gate references, and reviewer outcome in `final-review.md` with `apply_patch`.

- [ ] **Step 5: Completion rule**

  Claim P5 complete only if the final reviewer finds no unresolved issue and every automated and live gate is current for the exact final executable. Otherwise report P5 as pending with the first unmet gate and retain the safest closed guard compatible with the verified evidence.
