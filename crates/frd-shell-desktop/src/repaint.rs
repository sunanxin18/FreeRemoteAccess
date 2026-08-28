use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
pub(crate) struct RepaintScheduler {
    state: Arc<Mutex<RepaintState>>,
}

#[derive(Default)]
struct RepaintState {
    deadline: Option<RepaintPlan>,
    generation: u64,
    notification_pending: bool,
    shutdown: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RepaintPlan {
    pub(crate) generation: u64,
    pub(crate) deadline: Instant,
}

impl RepaintScheduler {
    pub(crate) fn request_after(&self, now: Instant, delay: Duration, notify: impl FnOnce()) {
        let requested = now.checked_add(delay).unwrap_or(now);
        let should_notify = {
            let mut state = self.lock();
            if state.shutdown
                || state
                    .deadline
                    .is_some_and(|current| current.deadline <= requested)
            {
                return;
            }
            state.generation = state.generation.saturating_add(1);
            state.deadline = Some(RepaintPlan {
                generation: state.generation,
                deadline: requested,
            });
            if state.notification_pending {
                false
            } else {
                state.notification_pending = true;
                true
            }
        };
        if should_notify {
            notify();
        }
    }

    pub(crate) fn take_plan(&self) -> Option<RepaintPlan> {
        let mut state = self.lock();
        state.notification_pending = false;
        (!state.shutdown).then_some(state.deadline).flatten()
    }

    pub(crate) fn fire(&self, plan: RepaintPlan, now: Instant) -> bool {
        let mut state = self.lock();
        if state.shutdown || now < plan.deadline || state.deadline != Some(plan) {
            return false;
        }
        state.deadline = None;
        true
    }

    pub(crate) fn shutdown(&self) {
        let mut state = self.lock();
        state.shutdown = true;
        state.deadline = None;
        state.notification_pending = false;
    }

    fn lock(&self) -> MutexGuard<'_, RepaintState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::RepaintScheduler;

    #[test]
    fn delayed_repaints_coalesce_to_one_wake_and_ignore_stale_or_shutdown_deadlines() {
        let scheduler = RepaintScheduler::default();
        let notifications = AtomicUsize::new(0);
        let now = Instant::now();

        for _ in 0..100 {
            scheduler.request_after(now, Duration::from_millis(50), || {
                notifications.fetch_add(1, Ordering::Relaxed);
            });
        }
        assert_eq!(notifications.load(Ordering::Relaxed), 1);
        let first = scheduler.take_plan().expect("one coalesced deadline");

        scheduler.request_after(now, Duration::from_millis(10), || {
            notifications.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(notifications.load(Ordering::Relaxed), 2);
        let replacement = scheduler
            .take_plan()
            .expect("earlier deadline replaces the plan");
        assert!(!scheduler.fire(first, now + Duration::from_millis(50)));
        assert!(scheduler.fire(replacement, now + Duration::from_millis(10)));

        scheduler.shutdown();
        scheduler.request_after(now, Duration::ZERO, || {
            notifications.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(notifications.load(Ordering::Relaxed), 2);
        assert!(scheduler.take_plan().is_none());
    }
}
