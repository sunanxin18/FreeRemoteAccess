# Apple HP 1440p Decode Throughput Design

## Goal

Run Apple High Performance sessions preferentially at 2560x1440 pixels,
2560x1440 points (scale 1) and 60 Hz, with a one-way automatic in-session
downgrade to the same scale-1 geometry at 30 Hz when sustained local decode and
presentation latency proves that the client cannot keep up.

## Product boundaries

- Apple wire behavior remains scoped to `apple_high_performance`; Standard/MVS
  and RDP are unchanged.
- The current initial HP attempt requests 2560x1440 pixels, 2560x1440 points
  (scale 1) at 60 Hz in the recovered 0x1d display-mode layout and sends
  Message `0x1c = 0x0d` for the 60-FPS capability.
- The fallback is available only after the HP session is confirmed. It sends
  exactly one encrypted 0x1d for the same scale-1 2560x1440 geometry at 30 Hz;
  it does not restart authentication and does not send a second `0x1c`.
  Standard/MVS retains `0x1c = 0x0c`.
- The earlier 1280x720 request, stock-selected 1312x848 result, and
  bit-clear-only capability candidate are superseded historical evidence, not
  the current HP contract; their chronology is retained in the validation
  record.
- UDP receive is never blocked as a form of backpressure. PLI remains a loss
  recovery mechanism, not a steady-state load-control mechanism.
- FFmpeg software decode is one backend behind the existing media API. Thread
  and SIMD choices may not leak into Apple, RDP, renderer, or UI modules.

## Decode policy

- The FFmpeg plugin uses one decode thread below 2560x1440 and two frame
  threads at or above 2560x1440 when the host exposes at least two logical
  processors.
- Two threads are a deliberate latency bound. Automatic all-core frame
  threading is forbidden because each extra frame thread adds pipeline delay.
- The native bridge records the selected thread count and active thread type
  for deterministic tests and diagnostics.
- The policy is OS-neutral. Windows, macOS, and Linux compile the same bridge;
  upstream libavcodec selects the platform thread implementation.

## Adaptive downgrade

- One isolated, protocol-neutral load controller observes media-ingress to
  present timing and decoder input queue depth. It cannot inspect or serialize
  Apple wire messages.
- A single late frame or RTP reorder event never triggers a downgrade. The
  controller requires both sustained latency above 200 ms and at least four
  queued access units for two continuous seconds.
- The downgrade is one-way for a connection attempt and only admits a
  confirmed HP session. It sends the same recovered
  `RFBSetDisplayConfiguration`/0x1d shape with the exact scale-1 2560x1440
  geometry and 30-Hz mode through the existing encrypted writer. This matches
  the recovered ARD dynamic-mode call path, does not restart authentication or
  duplicate credentials, and does not send another `0x1c`; the initial HP
  `0x1c = 0x0d` remains the negotiated capability. Standard/MVS remains
  `0x1c = 0x0c` and cannot receive this action.

## SIMD and build portability

- Windows FFmpeg builds require NASM and enable upstream x86 assembly instead
  of passing `--disable-x86asm`.
- The native plugin build supports Windows MSVC, macOS, and Linux through the
  Rust `cc` build dependency and links `avcodec`/`avutil` from `FFMPEG_DIR`.
- Non-x86 targets do not require NASM. FFmpeg uses its architecture-specific
  assembler/intrinsics when available; a portable-C FFmpeg remains compatible.
- LGPL, fixed-version, trusted-load, and corresponding-source packaging gates
  remain unchanged.

## Copy policy

The first implementation retains the current stable plugin ABI. Measurements
must separately report decode time and the two CPU plane-copy stages. A pooled
buffer change is allowed only after the 30-Hz/SIMD/two-thread build is measured;
zero-copy ownership requires a separately versioned ABI and is not smuggled into
this change.

## Acceptance

- Exact 0x1d tests prove explicit 60.0-Hz and 30.0-Hz encodings and the primary
  mode is 2560x1440 pixels / 2560x1440 points (1x scale).
- Standard/MVS startup geometry and wire builder tests remain unchanged.
- FFmpeg policy tests prove one-thread low-resolution and bounded two-thread
  1440p behavior.
- Native FFmpeg integration proves the optimized bundle decodes the existing
  Main444 fixture and exposes the requested thread configuration.
- Deterministic controller tests prove the two-signal/two-second threshold,
  immunity to transient spikes, one-way behavior, and Apple-HP-only command.
- Apple runtime tests prove the command writes the exact encrypted 30-Hz 0x1d
  once; RDP and Standard/MVS do not serialize it.
- Windows package verification remains trusted and LGPL-complete.
- Live validation records accepted ServerState geometry, observed RTP/AU rate,
  decode throughput, queue depth, and input-to-present latency; it does not
  claim success from compilation alone.
