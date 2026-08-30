# Task 5 smoke follow-up

## Scope

Migrated the missed `tools/frd-wgpu-smoke` startup caller from the removed serial renderer API to one atomic `FrameTransaction::Startup` passed to `RemoteRenderer::apply_update_batch`. No protocol, shell, UI, Task 6 selector, or plan change is included.

## RED / GREEN

- RED: `cargo test -p frd-wgpu-smoke -- --nocapture` failed with E0599 at `main.rs:98` because `RemoteRenderer::apply_update` no longer exists.
- Test-first RED: the existing fixture test was changed to require exactly one startup transaction and failed with E0432 (`smoke_transactions` missing) alongside the original E0599.
- GREEN: the existing fixture remains a 2x2 BGRX red/green/blue/white patch at generation/revision 1 with `FullBaseline`; the pending presentation receipt is still confirmed only by the existing compositor record/submit/present path.

## Verification

- `cargo test -p frd-wgpu-smoke -- --nocapture`: 1 passed.
- `cargo check -p frd-wgpu-smoke`: passed.
- `cargo test --workspace`: passed.
- `cargo test --workspace --no-default-features`: passed.
- `cargo fmt --all -- --check` and `git diff --check -- tools/frd-wgpu-smoke/src/main.rs`: passed.
- `rg -n "\bApplyOutcome\b|\.apply_update\(" tools/frd-wgpu-smoke crates/frd-render-wgpu crates/frd-shell-desktop apps/freeremotedesk-windows`: zero matches.

## Concerns

None in this follow-up. The concurrently modified Task 6 plan document is deliberately excluded from this commit.
