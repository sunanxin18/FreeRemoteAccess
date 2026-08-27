use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
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
    observer: Arc<GpuFaultObserver>,
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
        self: &Arc<Self>,
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
            observer: self.clone(),
            context_id,
            epoch,
        })
    }

    pub(crate) fn commit_if_unchanged<R>(
        self: &Arc<Self>,
        context_id: crate::GpuContextId,
        token: GpuCleanToken,
        commit: impl FnOnce() -> R,
    ) -> Result<R, GpuFaultClass> {
        if context_id != token.context_id || !Arc::ptr_eq(self, &token.observer) {
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
pub struct GpuFaultScope {
    device: wgpu::Device,
    observer: Arc<GpuFaultObserver>,
    context_id: crate::GpuContextId,
    start_epoch: u64,
    validation: Option<wgpu::ErrorScopeGuard>,
    internal: Option<wgpu::ErrorScopeGuard>,
    out_of_memory: Option<wgpu::ErrorScopeGuard>,
}

impl GpuFaultScope {
    pub(crate) fn new(
        device: wgpu::Device,
        observer: Arc<GpuFaultObserver>,
        context_id: crate::GpuContextId,
    ) -> Result<Self, GpuFaultClass> {
        let start_epoch = observer.begin_operation()?;
        let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        Ok(Self {
            device,
            observer,
            context_id,
            start_epoch,
            validation: Some(validation),
            internal: Some(internal),
            out_of_memory: Some(out_of_memory),
        })
    }

    pub fn finish(mut self) -> Result<GpuCleanToken, GpuFaultClass> {
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
        let poll_fault = self
            .device
            .poll(wgpu::PollType::Poll)
            .err()
            .map(|_| GpuFaultClass::Internal);
        let scope_fault = [
            poll_error_scope(validation),
            poll_error_scope(internal),
            poll_error_scope(out_of_memory),
        ]
        .into_iter()
        .chain([poll_fault])
        .flatten()
        .max_by_key(|fault| fault.priority());

        if let Some(fault) = scope_fault {
            self.observer.record(fault);
            return Err(self.observer.current().unwrap_or(fault));
        }
        self.observer.clean_token(self.context_id, self.start_epoch)
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
