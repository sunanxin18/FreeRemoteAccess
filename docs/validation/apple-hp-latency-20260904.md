# Apple High Performance Latency Validation (2026-09-04)

## Scope

This record covers one authorized stock-Mac interoperability target and the
Windows client. It does not claim arbitrary networks, displays, packet loss,
client platforms, or long-running rollover behavior. No helper, daemon, driver,
or other FreeRemoteDesk component was installed on the Mac.

The current installed Windows executable candidate for the current HP contract
has SHA-256:

`6F3368FE16D05246F54DC6713B0CE7EC3F98B5F508F31AEC2E33305F6DDF8E9A`

Earlier installed candidates are retained below only as historical identifiers
and are superseded: `518FAEF4DE502DC0E4CD13EFA5571F6F282611A05C3C6388BEE8106986DB056D`
was the earlier final candidate, and
`E0457D2C6BA352E5302F42B08C89162D1AA6CDAE57DA8F9902D77F45126C9678`
was the earlier scale-1 follow-up candidate.

The current installed package passed `verify-windows-package.ps1
-RequireTrustedInstall` with the fixed FFmpeg 8.1.2 DLL and license set.

## Chronology: superseded historical candidates

The original Apple HP session decoded `2560x1440` HEVC Main444 through the
FFmpeg software backend. The client also unconditionally advertised the Apple
Message `0x1c` stream-1 60-FPS capability. Incoming video was approximately 55
FPS while the decoder sustained approximately 50 FPS. The 64-access-unit
pre-decode FIFO therefore accumulated about two seconds before backpressure.

Queue backpressure requested a PLI and waited for a new IRAP. The tested Mac did
not provide an immediate IRAP for that path, so reducing the FIFO limit to eight
made the failure happen earlier. That candidate was rejected and fully reverted.

Static recovery of stock ScreenSharing established that Apple sets the
Message-`0x1c` 60-FPS bit only when the local HEVC decoder capability query for
the current pixel geometry returns at least 60 FPS. An earlier Windows Main444
software candidate left that bit clear; this is superseded historical evidence,
not the current HP startup contract (`0x1c = 0x0d`). An explicit
capability-aware encoder entry remains available for a future backend with
verified 60-FPS support.

The previous 308-byte `0x1d` builder was also not a valid display
configuration: it left `modeCount=0`. The recovered Apple layout is one
296-byte display record with five 28-byte modes. An earlier HP-only candidate
then sent a requested `1280x720` primary mode and Apple's four fallback modes;
the Mac selected the `1312x848` fallback, reducing decoded pixels from 3.69 MP
to 1.11 MP. That 1280x720/1312x848 candidate is superseded historical
evidence. Standard/MVS continues to use its previous ServerInit geometry and
`0x1c = 0x0c` builder path.

One remaining recovery tail came from scheduling PLI only at the next periodic
SRTCP report, up to one second later. A queued PLI now only advances the next
control-report deadline to the current instant. The existing next service loop
sends the same compound RR plus one coalesced PLI, clears it only after success,
and restores the ordinary one-second report period.

## Current HP wire contract (2026-09-04)

The current contract, reflected by the current source and focused tests, is:

- Initial HP: mode 0 requests 2560x1440 pixels and 2560x1440 points (scale 1)
  at 60 Hz in `0x1d`, with Message `0x1c = 0x0d`.
- Fallback: only after the HP session is confirmed, one exact same-geometry
  scale-1 2560x1440/30-Hz `0x1d` is written through the existing encrypted
  session. It does not restart authentication and does not send a second
  `0x1c`.
- Standard/MVS remains on its existing path with `0x1c = 0x0c`; it cannot
  receive the HP fallback action.

The older 1280x720 request, 1312x848 server selection, and bit-clear-only
candidate described above are superseded historical evidence and must not be
read as current geometry or capability behavior.

## Bounded Measurements: superseded historical candidates

| Candidate | Presented span | Input-to-present | Outcome |
| --- | ---: | --- | --- |
| Original full-size/60-FPS declaration | stopped after about 5 seconds of measured presentation | p50 46 ms, p95 80 ms | permanently stopped while TCP/UDP remained active |
| 60-FPS bit clear only | 8.54 seconds | p50 48 ms, p95 66 ms | full-size software decode still filled the queue and stopped |
| Valid HP display configuration, diagnostic build | 85.28 seconds | p50 32 ms, p95 919 ms | actual `1312x848`; no decode backpressure reset; one reorder/IRAP recovery produced the long tail |
| Final release with expedited PLI | 107.78 seconds at observation time and still advancing | p50 33 ms, p95 116 ms, max 692 ms | no permanent stop observed in the bounded run |

The final release sample contained 428 presentation events and 90
input-to-next-present samples. Input counters advanced equally through
`shell_physical_accepted`, `command_enqueued`, `runtime_consumed`, and
`writer_completed`, confirming that the earlier remote keyboard focus-domain
stall was caused by local keyboard-domain ownership rather than network write
delay.

The diagnostic build observed one `ReorderWindowExceeded`, one recovery-marker
resynchronization, and 34 access units dropped while awaiting an IRAP. Replay or
too-old packets were a small minority and were not the source of the sustained
two-second FIFO latency.

## Verification

The final source state passed:

- `cargo fmt --all -- --check`;
- `cargo test -p frd-protocol-apple`;
- `cargo test -p frd-ui-egui -p frd-shell-desktop` (`191` shell and `34` UI
  tests passed);
- `cargo build --release -p freeremotedesk-windows`;
- `git diff --check` (only the pre-existing PowerShell LF-to-CRLF notice).

The current focused protocol tests cover:

- HP scale-1 2560x1440 startup with explicit-60-FPS `0x0d`, plus the
  Standard/MVS capability-default `0x0c`;
- the complete 308-byte HP `0x1d` display record and all five modes;
- HP-only confirmed-session exact 30-Hz fallback with no authentication restart
  or second `0x1c`, with Standard/MVS isolation;
- 119-byte plus NUL display-name truncation matching Apple `strlcpy` behavior;
- immediate-next-loop PLI scheduling, coalescing, successful clearing, and
  restoration of the normal control-report period.

## Remaining Limits

- The Mac's earlier selection of Apple's `1312x848` fallback rather than the
  requested `1280x720` primary mode belongs to the superseded historical
  candidate; it is not evidence about the current 2560x1440 scale-1 contract.
- The current 2560x1440/60-Hz workload was bounded and did not continuously
  generate a changing full-screen frame at 60 FPS. It is not maximum decoder
  throughput evidence and does not close the planned AU-rate, queue-depth,
  decode-time, plane-copy, or presentation-series measurement gate.
- The sustained-overload controller did not fire in the recorded run, so no
  stock Mac application of the automatic 30-Hz fallback is claimed or
  verified. Live fallback behavior remains unverified.
- The run does not prove zero future reorder events or arbitrary packet-loss
  recovery. The measured worst input sample was 692 ms.
- Windows hardware Main444 remains unavailable on the tested D3D12 adapter;
  native hardware decoding is not claimed.
- Dynamic resize after connection remains a separate Apple HP generation and
  display-configuration feature. This change fixes initial source load only.

## Current 2560x1440 1x / 60-Hz follow-up

Later on 2026-09-04, the current installed Windows candidate
`6F3368FE16D05246F54DC6713B0CE7EC3F98B5F508F31AEC2E33305F6DDF8E9A`
used the recovered explicit current/preferred mode index `0` with mode 0 set to
2560x1440 pixels, 2560x1440 points, and 60.0 Hz. A read-only stock-macOS
`system_profiler SPDisplaysDataType -json` query during the authenticated HP
session reported the main online display as `2560 x 1440 @ 60.00Hz`, confirming
the requested 1x logical scale on this target. No server component was added or
modified.

The candidate used the fixed FFmpeg 8.1.2 LGPL bundle with x86 assembly enabled
and a bounded two-frame-thread policy for 2560x1440 in either orientation. The
installed hashes were:

- `avcodec-62.dll`:
  `574D4C1CB39F2E07A58780F324BFF7B9E47DFB02E133E231DC0471C4DECA040C`;
- `freeremotedesk_ffmpeg.dll`:
  `91FAB0EB8A6CBCDBAE14C294835B8C9F1E1D7B1DE372513663EBB391D2C09803`.

A bounded Finder-window motion workload from the earlier scale-1 follow-up
candidate produced 246 presentation records over
20.295 seconds. This is 12.07 observed presentations per second with frame-gap
p50 48.07 ms and p95 267.54 ms. The workload did not continuously generate a
new full-screen frame at 60 FPS, so these numbers are presentation observations,
not decoder maximum throughput. Process CPU increased by 23.97 CPU-seconds over
the interval (about 1.18 logical cores on average), working set was about 498
MiB, and macOS still reported 60 Hz afterwards. The sustained-overload
controller therefore did not apply the 30-Hz fallback in this run.

The measurement did not expose a separate AU-rate or copy-time series and
contains only one input-to-next-present sample. It is insufficient evidence for
a plugin frame-pool or zero-copy ABI change, so neither was added. A future
stress run must use a controlled 60-FPS changing source and record decoder queue
depth, AU ingress, decode completion, both CPU plane-copy stages, and present
completion independently.
