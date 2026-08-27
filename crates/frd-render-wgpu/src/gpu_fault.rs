use std::future::Future;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
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

pub(crate) struct GpuFaultObserver {
    epoch: AtomicU64,
    current: AtomicU8,
}

impl GpuFaultObserver {
    pub(crate) fn new() -> Self {
        Self {
            epoch: AtomicU64::new(0),
            current: AtomicU8::new(0),
        }
    }

    pub(crate) fn begin_operation(&self) -> Result<u64, GpuFaultClass> {
        let epoch = self.epoch.load(Ordering::Acquire);
        match self.current() {
            Some(fault) => Err(fault),
            None => Ok(epoch),
        }
    }

    pub(crate) fn record(&self, fault: GpuFaultClass) {
        let mut current = self.current.load(Ordering::Acquire);
        loop {
            let retained = decode_fault(current).filter(|seen| seen.priority() >= fault.priority());
            let next = retained.unwrap_or(fault) as u8;
            match self.current.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn fault_since(&self, epoch: u64) -> Option<GpuFaultClass> {
        (self.epoch.load(Ordering::Acquire) != epoch)
            .then(|| self.current())
            .flatten()
    }

    pub(crate) fn current(&self) -> Option<GpuFaultClass> {
        decode_fault(self.current.load(Ordering::Acquire))
    }

    #[cfg(test)]
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
}

fn decode_fault(value: u8) -> Option<GpuFaultClass> {
    match value {
        0 => None,
        1 => Some(GpuFaultClass::Validation),
        2 => Some(GpuFaultClass::OutOfMemory),
        3 => Some(GpuFaultClass::Internal),
        4 => Some(GpuFaultClass::DeviceLost),
        5 => Some(GpuFaultClass::ObservationIncomplete),
        _ => Some(GpuFaultClass::Internal),
    }
}

#[must_use = "GPU 错误作用域必须调用 finish 才能完成故障观测"]
pub struct GpuFaultScope {
    device: wgpu::Device,
    observer: Arc<GpuFaultObserver>,
    start_epoch: u64,
    validation: Option<wgpu::ErrorScopeGuard>,
    internal: Option<wgpu::ErrorScopeGuard>,
    out_of_memory: Option<wgpu::ErrorScopeGuard>,
}

impl GpuFaultScope {
    pub(crate) fn new(
        device: wgpu::Device,
        observer: Arc<GpuFaultObserver>,
    ) -> Result<Self, GpuFaultClass> {
        let start_epoch = observer.begin_operation()?;
        let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        Ok(Self {
            device,
            observer,
            start_epoch,
            validation: Some(validation),
            internal: Some(internal),
            out_of_memory: Some(out_of_memory),
        })
    }

    pub fn finish(mut self) -> Result<(), GpuFaultClass> {
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

        let fault = [self.observer.fault_since(self.start_epoch), scope_fault]
            .into_iter()
            .flatten()
            .max_by_key(|fault| fault.priority());
        if let Some(fault) = fault {
            self.observer.record(fault);
            return Err(fault);
        }
        if let Some(fault) = self.observer.fault_since(self.start_epoch) {
            return Err(fault);
        }
        Ok(())
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
