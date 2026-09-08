use frd_core::SessionId;
use frd_shell_desktop::{
    AudioOutputFactory, CompiledFrameDrain, FrameBatchMetricsSnapshot, FrameCompileFailure,
    SessionHost, WakeSink,
};
use std::{sync::Arc, time::Instant};

struct UnusedServices;
impl WakeSink for UnusedServices {
    fn wake(&self) -> Result<(), frd_protocol_api::ProtocolError> {
        panic!("无活动会话提取不得唤醒后台工作线程")
    }
}
impl AudioOutputFactory for UnusedServices {
    fn open(&self) -> Result<Box<dyn frd_media_api::AudioOutput>, frd_media_api::AudioOutputError> {
        panic!("无活动会话提取不得打开音频设备")
    }
}

#[test]
fn public_compiled_frame_api_drains_empty_host_without_platform_services() {
    let services = Arc::new(UnusedServices);
    let mut host = SessionHost::new(Vec::new(), services.clone(), services);
    assert!(!host.retire_frame_presentation(SessionId::allocate()));
    let before = Instant::now();
    let drain: CompiledFrameDrain = host.drain_frame_transactions().unwrap();
    let after = Instant::now();
    let metrics: FrameBatchMetricsSnapshot = drain.metrics();
    assert!(drain.transactions().is_empty());
    assert_eq!(metrics.source_update_count(), 0);
    assert_eq!(metrics.transaction_count(), 0);
    assert_eq!(metrics.oldest_age(), None);
    assert!((before..=after).contains(&metrics.batch_started_at()));
    let transactions: Vec<frd_frame::FrameTransaction> = drain.into_transactions();
    assert!(transactions.is_empty());
    assert!(host
        .drain_frame_transactions()
        .unwrap()
        .into_transactions()
        .is_empty());
    assert!(!host.retire_frame_presentation(SessionId::allocate()));

    // 外部调用方可命名失败类型并读取准确的协议无关错误/时间快照；无私有构造器。
    let _error_accessor: fn(&FrameCompileFailure) -> &frd_frame::FrameTransactionError =
        FrameCompileFailure::error;
    let _metrics_accessor: fn(&FrameCompileFailure) -> FrameBatchMetricsSnapshot =
        FrameCompileFailure::metrics;
}
