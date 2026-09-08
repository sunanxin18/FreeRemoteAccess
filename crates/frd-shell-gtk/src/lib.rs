//! GTK 4.14 GLArea 帧事务适配器；命令提交事件不是窗口呈现 ACK。
pub mod display_geometry;
pub mod input_keymap;
use frd_core::PixelSize;
use frd_frame::FrameTransaction;

pub const fn available() -> bool {
    cfg!(all(
        target_os = "linux",
        feature = "gtk-shell",
        any(
            target_arch = "x86",
            target_arch = "x86_64",
            target_arch = "aarch64"
        )
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitError {
    Empty,
    Busy,
    TooLarge,
    StartupRequired,
    Closed,
    ContextUnavailable,
}

/// 拒绝时保留调用者的完整批次，不能静默丢失增量事务。
#[derive(Debug)]
pub struct RejectedBatch {
    pub reason: SubmitError,
    pub transactions: Vec<FrameTransaction>,
}

#[cfg_attr(not(all(target_os = "linux", feature = "gtk-shell")), allow(dead_code))]
struct PendingBatch {
    transactions: Option<Vec<FrameTransaction>>,
    startup_required: bool,
    closed: bool,
}
impl Default for PendingBatch {
    fn default() -> Self {
        Self {
            transactions: None,
            startup_required: true,
            closed: false,
        }
    }
}
#[cfg_attr(not(all(target_os = "linux", feature = "gtk-shell")), allow(dead_code))]
impl PendingBatch {
    fn push(&mut self, transactions: Vec<FrameTransaction>) -> Result<(), RejectedBatch> {
        let reason = if self.closed {
            Some(SubmitError::Closed)
        } else if transactions.is_empty() {
            Some(SubmitError::Empty)
        } else if self.transactions.is_some() {
            Some(SubmitError::Busy)
        } else if self.startup_required
            && !matches!(transactions[0], FrameTransaction::Startup { .. })
        {
            Some(SubmitError::StartupRequired)
        } else {
            let bytes = transactions.iter().try_fold(0usize, |total, tx| {
                let revision = match tx {
                    FrameTransaction::Startup { revision, .. }
                    | FrameTransaction::Revision { revision, .. } => revision,
                };
                revision
                    .patches
                    .iter()
                    .try_fold(total, |sum, patch| sum.checked_add(patch.pixels.len()))
            });
            if transactions.len() > 4096 || bytes.is_none_or(|n| n > 256 * 1024 * 1024) {
                Some(SubmitError::TooLarge)
            } else {
                None
            }
        };
        if let Some(reason) = reason {
            return Err(RejectedBatch {
                reason,
                transactions,
            });
        }
        self.transactions = Some(transactions);
        Ok(())
    }
    fn take(&mut self) -> Option<Vec<FrameTransaction>> {
        self.transactions.take()
    }
    fn applied(&mut self) {
        self.startup_required = false;
    }
    fn invalidate(&mut self) -> usize {
        self.startup_required = true;
        self.transactions.take().map_or(0, |batch| batch.len())
    }
}

#[cfg_attr(not(all(target_os = "linux", feature = "gtk-shell")), allow(dead_code))]
fn drawable_size(width: i32, height: i32, scale: i32) -> Option<PixelSize> {
    if width <= 0 || height <= 0 || scale <= 0 {
        return None;
    }
    let width = u32::try_from(width)
        .ok()?
        .checked_mul(u32::try_from(scale).ok()?)?;
    let height = u32::try_from(height)
        .ok()?
        .checked_mul(u32::try_from(scale).ok()?)?;
    if width > i32::MAX as u32 || height > i32::MAX as u32 {
        return None;
    }
    PixelSize::new(width, height)
}

#[cfg(all(
    target_os = "linux",
    feature = "gtk-shell",
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
mod adapter;
#[cfg(all(
    target_os = "linux",
    feature = "gtk-shell",
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
pub use adapter::{AdapterError, AdapterEvent, GtkFrameArea};

#[cfg(test)]
mod tests {
    use super::*;
    use frd_core::SessionId;
    use frd_frame::{FrameCompleteness, FrameReset, FrameRevision, PixelFormat};
    fn transaction(startup: bool) -> FrameTransaction {
        let session_id = SessionId::allocate();
        let revision = FrameRevision {
            session_id,
            generation: 1,
            revision: 1,
            patches: vec![],
            completeness: FrameCompleteness::FullBaseline,
        };
        let earliest_constituent_enqueue_at = std::time::Instant::now();
        if startup {
            FrameTransaction::Startup {
                earliest_constituent_enqueue_at,
                reset: FrameReset {
                    session_id,
                    generation: 1,
                    size: PixelSize::new(1, 1).unwrap(),
                    format: PixelFormat::Bgrx8UnormSrgb,
                },
                revision,
            }
        } else {
            FrameTransaction::Revision {
                earliest_constituent_enqueue_at,
                revision,
            }
        }
    }
    #[test]
    fn queue_returns_busy_batch_and_preserves_original() {
        let mut queue = PendingBatch::default();
        queue.push(vec![transaction(true)]).unwrap();
        let rejected = queue
            .push(vec![transaction(true), transaction(false)])
            .unwrap_err();
        assert_eq!(rejected.reason, SubmitError::Busy);
        assert_eq!(rejected.transactions.len(), 2);
        assert_eq!(queue.take().unwrap().len(), 1);
    }
    #[test]
    fn context_invalidation_discards_pending_and_requires_new_startup() {
        let mut queue = PendingBatch::default();
        assert_eq!(
            queue.push(vec![transaction(false)]).unwrap_err().reason,
            SubmitError::StartupRequired
        );
        queue.push(vec![transaction(true)]).unwrap();
        queue.take();
        queue.applied();
        queue.push(vec![transaction(false)]).unwrap();
        assert_eq!(queue.invalidate(), 1);
        assert!(queue.take().is_none());
        assert_eq!(
            queue.push(vec![transaction(false)]).unwrap_err().reason,
            SubmitError::StartupRequired
        );
        queue.push(vec![transaction(true)]).unwrap();
    }
    #[test]
    fn queue_bounds_and_closed_state_do_not_accept_work() {
        let mut queue = PendingBatch::default();
        assert_eq!(queue.push(vec![]).unwrap_err().reason, SubmitError::Empty);
        assert_eq!(
            queue
                .push((0..4097).map(|_| transaction(true)).collect())
                .unwrap_err()
                .reason,
            SubmitError::TooLarge
        );
        queue.closed = true;
        assert_eq!(
            queue.push(vec![transaction(true)]).unwrap_err().reason,
            SubmitError::Closed
        );
    }
    #[test]
    fn physical_geometry_uses_scale_and_rejects_overflow() {
        assert_eq!(drawable_size(400, 300, 2), PixelSize::new(800, 600));
        for args in [(0, 1, 1), (1, 1, 0), (-1, 1, 2), (i32::MAX, 2, 2)] {
            assert!(drawable_size(args.0, args.1, args.2).is_none());
        }
    }
}

#[cfg(all(
    target_os = "linux",
    feature = "gtk-shell",
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
mod runner;
#[cfg(all(
    target_os = "linux",
    feature = "gtk-shell",
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
pub use runner::{GtkRunner, GtkRunnerStores};

#[cfg(all(
    target_os = "linux",
    feature = "gtk-shell",
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
mod presentation;
#[cfg(all(
    target_os = "linux",
    feature = "gtk-shell",
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
pub use presentation::{SubmissionDiagnostics, SubmissionError, WindowSubmission};
