# Task 5 desktop shell atomic batch integration report

## Status and scope

- Status: complete.
- Base: `062a9f787491a0c17f0c4d501096409c0bd30fe7`.
- Scope stayed inside the Task 5 brief: desktop-shell compiler/batch-renderer integration, renderer serial-boundary removal, metrics/fatal integration, offline texture/device-recovery migration, and focused tests. No Task 6/7, protocol, CSV schema, comparison-script, or UI-layout work was added.

## RED

The brief's focused tests were written before their production seams existed. Each command exited 1 for the expected unresolved runtime/compiler/batch/fatal contract rather than an unrelated failure:

```text
cargo test -p frd-shell-desktop event_only_wake_ -- --nocapture
cargo test -p frd-shell-desktop reset_only_and_reset_damage_ -- --nocapture
cargo test -p frd-shell-desktop atomic_startup_full_baseline_ -- --nocapture
cargo test -p frd-shell-desktop batch_metric_context_ -- --nocapture
cargo test -p frd-shell-desktop candidate_metric_row_ -- --nocapture
cargo test -p frd-shell-desktop fatal_wake_ -- --nocapture
cargo test -p frd-shell-desktop fatal_redraw_requested_ -- --nocapture
cargo test -p frd-shell-desktop frame_batch_longest_fixed_schema_ -- --nocapture
```

RED established the missing `PendingLiveSessionPorts` acceptance boundary, real `SessionHost` compiler drain, atomic batch acceptance, separate UI/frame redraw facts, full metrics observers, closed fatal encoders, and fatal callback termination behavior.

## GREEN

- The eight focused RED commands now pass.
- Added direct contract coverage for one renderer call per nonempty compiled drain and fatal ordering (`frame_batch_failure_blocks_detaches_and_clears_in_exact_order`).
- `cargo test -p frd-shell-desktop -- --nocapture`: 91 passed, 0 failed.
- `cargo test -p frd-render-wgpu -- --nocapture`: 27 passed, 0 failed.
- `cargo fmt --all -- --check`, `cargo check -p frd-shell-desktop`, `cargo check -p frd-render-wgpu`, and `git diff --check`: pass.

## Interfaces and call points

- `BackgroundLaunchResult::Started` carries `PendingLiveSessionPorts`, which cannot own a compiler. Only `PendingLiveSessionPorts::accept`, called by the non-cancelled `SessionHost::accept_launch_outcome` branch immediately before installing `active`, constructs `FrameTransactionCompiler` in `LiveSessionPorts`.
- `SessionHost::drain_frame_transactions` drains one nonempty mailbox envelope set and calls `FrameTransactionCompiler::compile` exactly once. It captures complete source-update, transaction, oldest-age, and batch-start context for success and failure.
- `DesktopApplication::drain_runtime` reduces session events first, compiles once, skips the renderer for an empty transaction vector, and calls `RemoteRenderer::apply_update_batch` exactly once for a nonempty vector.
- `accept_batch_outcome` installs `RemoteBinding` only from `BatchApplyOutcome::installed_surface`. `final_boundary` only contributes the frame-redraw fact; it never publishes `FramePresented` or confirms a receipt.
- `FramePresented` remains owned by the actual compositor render path after record/submit/present succeeds.
- Offline startup texture and device-recovery restoration now each build the atomic startup transaction and call `apply_update_batch`; both preserve recovery behavior while using the same installed-surface acceptance rule.
- UI redraw and frame redraw are separate `RuntimeDrainOutcome` facts. Event-only Wake requests one UI redraw; reset-only and reset-plus-damage without a final boundary request no frame redraw.

## Metrics and fatal ordering

- Successful batch observation consumes `BatchMetricContext`, full `BatchApplyOutcome`, and actual `BatchScopeDiagnostics`; it emits a successful `CandidateBatch` with observed source/transaction/rectangle/scope/GPU facts.
- Compile and renderer failures emit `StableFault`, not failed `CandidateBatch`. Renderer failures retain full identity, primary/secondary error, actual optional scope observation, and GPU fault classification.
- Compile/renderer fatal transition order is exactly: block and release remote input -> detach renderer/remote binding -> clear `PendingTextureWrites` -> construct/dispatch fatal termination.
- Both Wake and RedrawRequested fatal branches terminate without requesting redraw or entering record/submit/present/receipt publication.
- Frame-batch fatal details use a fixed six-key closed schema and the longest-field regression stays within the safe-detail bound without truncation.

## Serial API removal audit

Legacy boundary search:

```text
rg -n "\bApplyOutcome\b|apply_update\(" crates apps src
```

Result: zero matches.

Remaining `apply_update_batch` production call points are the runtime nonempty compiled drain, offline startup texture, device recovery, and `RemoteRenderer::apply_update_batch` itself. The renderer API contract test also references the batch boundary. There are no serial renderer callers or exported serial API.

## Self-review and concerns

- Reviewed compiler ownership, compiler/renderer call cardinality, reset/no-boundary redraw behavior, installed-surface binding, deferred presentation confirmation, failure metric event kind/context, fatal teardown order, Wake/RedrawRequested fatal termination, offline startup, and device recovery against the Task 5 brief.
- Repository formatting, affected-crate checks/tests, diff whitespace, and legacy serial API searches are clean.
- Known concerns: none inside Task 5. Task 6/7 performance comparison and any live latency evidence remain deliberately out of scope.
