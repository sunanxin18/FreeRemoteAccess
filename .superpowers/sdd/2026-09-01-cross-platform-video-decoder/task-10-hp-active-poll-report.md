# Task 10 addendum report: Apple HP active-media poll cadence

## Scope

The Apple product runtime keeps its 100ms TCP application-frame poll through
startup and Message 1. Only `apple-high-performance` changes it to the named
5ms active-media cadence after its Message 2 transport activation succeeds.
The update occurs before the next blocking application-frame read and is
guarded by the current cadence state, so it is not repeated on Active loops.
Apple Standard/TCP-MVS continues with 100ms.

The 5ms value is a capture-proven direct-HP comparison value. It is defined in
the clean runtime file for this task and does not reference the user's
uncommitted `hpss.rs` work.

## TDD evidence

RED was recorded before the production change:

```text
cargo test -p frd-protocol-apple product_high_performance_uses_active_media_poll_before_its_next_blocking_read_once -- --nocapture
...
会话循环必须在测试期限内达到读边界: Timeout
test result: FAILED. 0 passed; 1 failed
```

The focused real TCP harness records the actual timeout update and the timeout
held at each blocking application-frame read. It proves 100ms before Message 2,
the single 5ms update after Message 2, ordering before the next read, and two
subsequent 5ms reads without a second update. A separate Standard session
proves that media activation retains 100ms and observes no 5ms update.

Mutation checks were also run: removing the active transition reproduced the
RED timeout; temporarily reapplying 5ms on every Active loop failed the
one-shot assertion with `left: 3`, `right: 1`. Both temporary mutations were
removed before final gates.

## Final verification

All commands exited 0 in this worktree:

```text
cargo test -p frd-protocol-apple product_high_performance_uses_active_media_poll_before_its_next_blocking_read_once -- --nocapture
cargo test -p frd-protocol-apple standard_media_activation_retains_the_hundred_millisecond_runtime_poll -- --nocapture
cargo test -p frd-protocol-apple
cargo test -p frd-protocol-apple --features mvs-profile
cargo fmt --all -- --check
cargo build -p freeremotedesk-windows
git diff --check
```

## Boundaries and concerns

- No wire, decoder, renderer, retry, fallback, media-datagram-budget, or
  startup-sleep behavior changed.
- Existing user WIP in `README.md`, `crates/frd-protocol-apple/src/hpss.rs`,
  `src/main.rs`, and `docs/validation/apple-dual-mode-blockers-20260901.md`
  was not modified or staged.
- This is offline/runtime-harness evidence only. It does not replace a fresh
  authenticated Mac interoperability run that verifies continued desktop
  refresh after first frame.
