use std::future::Future;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum GpuFaultClass {
    Validation = 1,
    OutOfMemory = 2,
    Internal = 3,
    DeviceLost = 4,
    ObservationIncomplete = 5,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GpuScopeObservation {
    pub begins: u64,
    pub finishes: u64,
    pub polls: u64,
}

impl GpuScopeObservation {
    pub fn checked_delta(self, earlier: Self) -> Option<Self> {
        Some(Self {
            begins: self.begins.checked_sub(earlier.begins)?,
            finishes: self.finishes.checked_sub(earlier.finishes)?,
            polls: self.polls.checked_sub(earlier.polls)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeLifecycleEvent {
    Begin,
    Finish,
    Poll,
}

pub(crate) trait ScopeLifecycleObserver: Send + Sync {
    fn record(&self, event: ScopeLifecycleEvent);
    fn snapshot(&self) -> GpuScopeObservation;
}

#[derive(Default)]
pub(crate) struct AtomicScopeLifecycleObserver {
    begins: AtomicU64,
    finishes: AtomicU64,
    polls: AtomicU64,
}

impl ScopeLifecycleObserver for AtomicScopeLifecycleObserver {
    fn record(&self, event: ScopeLifecycleEvent) {
        let counter = match event {
            ScopeLifecycleEvent::Begin => &self.begins,
            ScopeLifecycleEvent::Finish => &self.finishes,
            ScopeLifecycleEvent::Poll => &self.polls,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> GpuScopeObservation {
        GpuScopeObservation {
            begins: self.begins.load(Ordering::Relaxed),
            finishes: self.finishes.load(Ordering::Relaxed),
            polls: self.polls.load(Ordering::Relaxed),
        }
    }
}

pub(crate) struct ObservedScopeLifecycle<'a> {
    observer: &'a dyn ScopeLifecycleObserver,
    finish_recorded: bool,
    poll_recorded: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeLifecycleError {
    DuplicateFinish,
    PollBeforeFinish,
    DuplicatePoll,
}

pub(crate) fn begin_observed_scope<T, E>(
    observer: &dyn ScopeLifecycleObserver,
    acquire: impl FnOnce() -> Result<T, E>,
) -> Result<(T, ObservedScopeLifecycle<'_>), E> {
    let acquired = acquire()?;
    observer.record(ScopeLifecycleEvent::Begin);
    Ok((
        acquired,
        ObservedScopeLifecycle {
            observer,
            finish_recorded: false,
            poll_recorded: false,
        },
    ))
}

impl ObservedScopeLifecycle<'_> {
    pub(crate) fn record_finish(&mut self) -> Result<(), ScopeLifecycleError> {
        if self.finish_recorded {
            return Err(ScopeLifecycleError::DuplicateFinish);
        }
        self.observer.record(ScopeLifecycleEvent::Finish);
        self.finish_recorded = true;
        Ok(())
    }

    pub(crate) fn record_poll(&mut self) -> Result<(), ScopeLifecycleError> {
        if !self.finish_recorded {
            return Err(ScopeLifecycleError::PollBeforeFinish);
        }
        if self.poll_recorded {
            return Err(ScopeLifecycleError::DuplicatePoll);
        }
        self.observer.record(ScopeLifecycleEvent::Poll);
        self.poll_recorded = true;
        Ok(())
    }
}

impl GpuFaultClass {
    pub(crate) fn from_wgpu_error(error: &wgpu::Error) -> Self {
        match error {
            wgpu::Error::Validation { .. } => Self::Validation,
            wgpu::Error::OutOfMemory { .. } => Self::OutOfMemory,
            wgpu::Error::Internal { .. } => Self::Internal,
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::DeviceLost => 5,
            Self::OutOfMemory => 4,
            Self::Internal => 3,
            Self::Validation => 2,
            Self::ObservationIncomplete => 1,
        }
    }
}

#[derive(Clone, Copy)]
struct GpuFaultState {
    epoch: u64,
    current: Option<GpuFaultClass>,
}

pub(crate) struct GpuFaultObserver {
    state: Mutex<GpuFaultState>,
}

pub struct GpuCleanToken {
    observer_identity: usize,
    context_id: crate::GpuContextId,
    epoch: u64,
}

impl GpuFaultObserver {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(GpuFaultState {
                epoch: 0,
                current: None,
            }),
        }
    }

    pub(crate) fn begin_operation(&self) -> Result<u64, GpuFaultClass> {
        let state = self.lock_state();
        match state.current {
            Some(fault) => Err(fault),
            None => Ok(state.epoch),
        }
    }

    pub(crate) fn record(&self, fault: GpuFaultClass) {
        self.record_with(fault, || {});
    }

    pub(crate) fn current(&self) -> Option<GpuFaultClass> {
        self.lock_state().current
    }

    pub(crate) fn clean_token(
        &self,
        context_id: crate::GpuContextId,
        epoch: u64,
    ) -> Result<GpuCleanToken, GpuFaultClass> {
        let state = self.lock_state();
        if let Some(fault) = state.current {
            return Err(fault);
        }
        if state.epoch != epoch {
            return Err(GpuFaultClass::ObservationIncomplete);
        }
        Ok(GpuCleanToken {
            observer_identity: self as *const Self as usize,
            context_id,
            epoch,
        })
    }

    pub(crate) fn commit_if_unchanged<R>(
        &self,
        context_id: crate::GpuContextId,
        token: GpuCleanToken,
        commit: impl FnOnce() -> R,
    ) -> Result<R, GpuFaultClass> {
        if context_id != token.context_id || token.observer_identity != self as *const Self as usize
        {
            return Err(GpuFaultClass::Internal);
        }
        let state = self.lock_state();
        if let Some(fault) = state.current {
            drop(state);
            return Err(fault);
        }
        if state.epoch != token.epoch {
            drop(state);
            return Err(GpuFaultClass::ObservationIncomplete);
        }
        let result = commit();
        drop(state);
        Ok(result)
    }

    fn record_with(&self, fault: GpuFaultClass, after_publication: impl FnOnce()) {
        let mut state = self.lock_state();
        let retained = state
            .current
            .filter(|seen| seen.priority() >= fault.priority());
        state.current = Some(retained.unwrap_or(fault));
        state.epoch = state.epoch.saturating_add(1);
        after_publication();
    }

    fn lock_state(&self) -> MutexGuard<'_, GpuFaultState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poison) => {
                let mut state = poison.into_inner();
                state.current = Some(GpuFaultClass::Internal);
                state.epoch = state.epoch.saturating_add(1);
                state
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn record_with_publication_paused(
        &self,
        fault: GpuFaultClass,
        after_publication: impl FnOnce(),
    ) {
        self.record_with(fault, after_publication);
    }

    #[cfg(test)]
    pub(crate) fn epoch(&self) -> u64 {
        self.lock_state().epoch
    }
}

#[must_use = "GPU 错误作用域必须调用 finish 才能完成故障观测"]
pub struct GpuFaultScope<'a> {
    device: &'a wgpu::Device,
    observer: &'a GpuFaultObserver,
    context_id: crate::GpuContextId,
    start_epoch: u64,
    validation: Option<wgpu::ErrorScopeGuard>,
    internal: Option<wgpu::ErrorScopeGuard>,
    out_of_memory: Option<wgpu::ErrorScopeGuard>,
    lifecycle: ObservedScopeLifecycle<'a>,
}

impl<'a> GpuFaultScope<'a> {
    pub(crate) fn new(
        device: &'a wgpu::Device,
        observer: &'a GpuFaultObserver,
        lifecycle_observer: &'a dyn ScopeLifecycleObserver,
        context_id: crate::GpuContextId,
    ) -> Result<Self, GpuFaultClass> {
        let start_epoch = observer.begin_operation()?;
        let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let (_, lifecycle) =
            begin_observed_scope(lifecycle_observer, || Ok::<(), GpuFaultClass>(()))?;
        Ok(Self {
            device,
            observer,
            context_id,
            start_epoch,
            validation: Some(validation),
            internal: Some(internal),
            out_of_memory: Some(out_of_memory),
            lifecycle,
        })
    }

    pub fn finish(mut self) -> Result<GpuCleanToken, GpuFaultClass> {
        let finish_observation = self.lifecycle.record_finish();
        let validation = self
            .validation
            .take()
            .expect("validation scope exists")
            .pop();
        let internal = self.internal.take().expect("internal scope exists").pop();
        let out_of_memory = self
            .out_of_memory
            .take()
            .expect("out-of-memory scope exists")
            .pop();

        // wgpu 30 的错误作用域是线程局部且按 LIFO 弹出；原生后端的 pop future
        // 在弹出时已就绪。Poll 只驱动回调/错误观测，不等待队列完成，因此每帧不会
        // 引入 Wait/Fence；若后端仍返回 Pending，则保守地拒绝确认本帧。
        let poll_observation = self.lifecycle.record_poll();
        let poll_fault = self
            .device
            .poll(wgpu::PollType::Poll)
            .err()
            .map(|_| GpuFaultClass::Internal);
        let lifecycle_fault = (finish_observation.is_err() || poll_observation.is_err())
            .then_some(GpuFaultClass::ObservationIncomplete);
        let scope_fault = [
            poll_error_scope(validation),
            poll_error_scope(internal),
            poll_error_scope(out_of_memory),
        ]
        .into_iter()
        .chain([poll_fault, lifecycle_fault])
        .flatten()
        .max_by_key(|fault| fault.priority());

        if let Some(fault) = scope_fault {
            self.observer.record(fault);
            return Err(self.observer.current().unwrap_or(fault));
        }
        self.observer.clean_token(self.context_id, self.start_epoch)
    }
}

/// 在作用域成功开始后运行操作，并在传播 panic 前显式尝试完成作用域。
/// 此函数仅为相邻 compositor crate 复用同一 GPU 作用域生命周期原语而公开。
#[doc(hidden)]
pub fn complete_scope_before_resuming_unwind<S, O, F>(
    scope: S,
    operation: impl FnOnce() -> O,
    finish: impl FnOnce(S) -> F,
) -> (F, O) {
    let operation = catch_unwind(AssertUnwindSafe(operation));
    let finish = catch_unwind(AssertUnwindSafe(|| finish(scope)));

    match (operation, finish) {
        (Ok(operation), Ok(finish)) => (finish, operation),
        (Err(operation_panic), Ok(finish)) => {
            drop(finish);
            resume_unwind(operation_panic)
        }
        (Ok(operation), Err(finish_panic)) => {
            drop(operation);
            resume_unwind(finish_panic)
        }
        (Err(operation_panic), Err(finish_panic)) => {
            // operation panic 是原始语义故障。遗忘第二个 payload，避免其析构在
            // 恢复原始展开期间再次 panic。
            std::mem::forget(finish_panic);
            resume_unwind(operation_panic)
        }
    }
}

fn poll_error_scope(future: impl Future<Output = Option<wgpu::Error>>) -> Option<GpuFaultClass> {
    let mut future = std::pin::pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(Some(error)) => Some(GpuFaultClass::from_wgpu_error(&error)),
        Poll::Ready(None) => None,
        Poll::Pending => Some(GpuFaultClass::ObservationIncomplete),
    }
}

#[cfg(test)]
mod linearization_tests {
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;

    use crate::GpuContextId;

    use super::{GpuFaultClass, GpuFaultObserver};

    #[test]
    fn fault_publication_at_the_old_split_blocks_reset_ownership_and_state_commit() {
        let context_id = GpuContextId(41);
        let observer = Arc::new(GpuFaultObserver::new());
        let epoch = observer.begin_operation().unwrap();
        let token = observer.clean_token(context_id, epoch).unwrap();
        let reset_state = Arc::new(Mutex::new((1_u64, "old-texture-owner")));
        let published = Arc::new(Barrier::new(2));
        let commit_ready = Arc::new(Barrier::new(2));
        let release_publication = Arc::new(Barrier::new(2));

        let fault_thread = {
            let observer = observer.clone();
            let published = published.clone();
            let release_publication = release_publication.clone();
            thread::spawn(move || {
                observer.record_with_publication_paused(GpuFaultClass::Validation, || {
                    published.wait();
                    release_publication.wait();
                });
            })
        };
        published.wait();

        let commit_thread = {
            let observer = observer.clone();
            let reset_state = reset_state.clone();
            let commit_ready = commit_ready.clone();
            thread::spawn(move || {
                commit_ready.wait();
                observer.commit_if_unchanged(context_id, token, || {
                    *reset_state.lock().unwrap() = (2, "new-texture-owner");
                })
            })
        };

        commit_ready.wait();
        release_publication.wait();
        fault_thread.join().unwrap();
        assert_eq!(
            commit_thread.join().unwrap(),
            Err(GpuFaultClass::Validation)
        );
        assert_eq!(*reset_state.lock().unwrap(), (1, "old-texture-owner"));
    }
}

#[cfg(test)]
mod scope_lifecycle_tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    use super::{
        begin_observed_scope, complete_scope_before_resuming_unwind, GpuScopeObservation,
        ScopeLifecycleEvent, ScopeLifecycleObserver,
    };

    #[derive(Default)]
    struct RecordingObserver(Mutex<Vec<ScopeLifecycleEvent>>);

    impl ScopeLifecycleObserver for RecordingObserver {
        fn record(&self, event: ScopeLifecycleEvent) {
            self.0.lock().unwrap().push(event);
        }

        fn snapshot(&self) -> GpuScopeObservation {
            let events = self.0.lock().unwrap();
            GpuScopeObservation {
                begins: events
                    .iter()
                    .filter(|event| **event == ScopeLifecycleEvent::Begin)
                    .count() as u64,
                finishes: events
                    .iter()
                    .filter(|event| **event == ScopeLifecycleEvent::Finish)
                    .count() as u64,
                polls: events
                    .iter()
                    .filter(|event| **event == ScopeLifecycleEvent::Poll)
                    .count() as u64,
            }
        }
    }

    #[test]
    fn scope_lifecycle_seam_records_begin_finish_poll_in_order() {
        let observer = Arc::new(RecordingObserver::default());
        let before = observer.snapshot();
        let (_, mut lifecycle) = begin_observed_scope(observer.as_ref(), || Ok::<_, ()>(()))
            .expect("acquisition succeeds");
        lifecycle.record_finish().unwrap();
        lifecycle.record_poll().unwrap();

        assert_eq!(
            *observer.0.lock().unwrap(),
            [
                ScopeLifecycleEvent::Begin,
                ScopeLifecycleEvent::Finish,
                ScopeLifecycleEvent::Poll
            ]
        );
        assert_eq!(
            observer.snapshot().checked_delta(before),
            Some(GpuScopeObservation {
                begins: 1,
                finishes: 1,
                polls: 1
            })
        );
    }

    #[test]
    fn scope_lifecycle_failed_begin_records_nothing() {
        let observer = Arc::new(RecordingObserver::default());
        let result = begin_observed_scope(observer.as_ref(), || Err::<(), _>("acquire"));
        assert!(result.is_err());
        assert!(observer.0.lock().unwrap().is_empty());
    }

    #[test]
    fn operation_panic_remains_primary_when_scope_finish_also_panics() {
        const OPERATION_PANIC: &str = "operation panic";
        const FINISH_PANIC: &str = "finish panic";

        let finish_calls = Rc::new(Cell::new(0));
        let observed_finish_calls = finish_calls.clone();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            complete_scope_before_resuming_unwind(
                (),
                || std::panic::panic_any(OPERATION_PANIC),
                |_| {
                    observed_finish_calls.set(observed_finish_calls.get() + 1);
                    std::panic::panic_any(FINISH_PANIC);
                },
            )
        }))
        .unwrap_err();

        assert_eq!(panic.downcast_ref::<&str>(), Some(&OPERATION_PANIC));
        assert_eq!(finish_calls.get(), 1);
    }
}
