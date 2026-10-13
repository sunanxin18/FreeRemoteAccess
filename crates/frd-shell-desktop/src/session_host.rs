//! 桌面组合入口：共享执行器通过注入使用现有 FFmpeg 后端。

use std::sync::Arc;

use frd_core::{ProtocolId, SessionId, TargetSystem};
use frd_frame::SurfaceUpdate;
use frd_media_api::{VideoDecoderRegistry, VideoStreamIdentity};
use frd_protocol_api::ProtocolFactory;
use frd_protocol_api::{ConnectRequest, SessionCommand, SessionEvent};
use frd_session::{CleanupComplete, SessionStartPermit};
use frd_shell_runtime::{
    BackgroundCleanupFailure, BackgroundCleanupOutcome, VideoDecodeLoadSnapshot, VideoFrameToken,
    VideoStreamAdmission, VideoWorkerEvent,
};

#[cfg(test)]
pub(crate) use frd_shell_runtime::session_host::{test_support, TestLaunchOutcome};
pub use frd_shell_runtime::session_host::{
    AcceptedLaunchOutcome, AudioOutputFactory, BackgroundLaunchOutcome, CompiledFrameDrain,
    FrameCompileFailure, SessionHostError, WakeSink,
};

/// 桌面壳的既有构造接口；全部会话状态由共享执行器持有。
pub struct SessionHost(frd_shell_runtime::SessionHost);

fn desktop_video_registry_loader() -> frd_shell_runtime::StreamRegistryLoader {
    Arc::new(|_identity| {
        frd_video_ffmpeg::FfmpegBackend::load()
            .map(|backend| VideoDecoderRegistry::new(vec![Box::new(backend)]))
    })
}

impl SessionHost {
    pub fn new(
        factories: impl IntoIterator<Item = Arc<dyn ProtocolFactory>>,
        wake: Arc<dyn WakeSink>,
        audio_factory: Arc<dyn AudioOutputFactory>,
    ) -> Self {
        Self(frd_shell_runtime::SessionHost::new(
            factories,
            wake,
            audio_factory,
            desktop_video_registry_loader(),
        ))
    }

    #[cfg(test)]
    pub(crate) fn new_with_test_spawner(
        factories: impl IntoIterator<Item = Arc<dyn ProtocolFactory>>,
        wake: Arc<dyn WakeSink>,
        audio_factory: Arc<dyn AudioOutputFactory>,
        worker_spawner: Arc<dyn test_support::WorkerSpawner>,
    ) -> Self {
        Self(frd_shell_runtime::SessionHost::new_with_test_spawner(
            factories,
            wake,
            audio_factory,
            worker_spawner,
            desktop_video_registry_loader(),
        ))
    }

    pub fn begin_launch(
        &mut self,
        permit: SessionStartPermit,
        target: TargetSystem,
        request: ConnectRequest,
        notify: impl Fn(BackgroundLaunchOutcome) + Send + Sync + 'static,
    ) -> Result<bool, SessionHostError> {
        self.0.begin_launch(permit, target, request, notify)
    }

    pub fn cancel_pending_launch(&mut self) -> bool {
        self.0.cancel_pending_launch()
    }

    pub fn launch_is_pending(&self) -> bool {
        self.0.launch_is_pending()
    }

    pub fn accept_launch_outcome(
        &mut self,
        outcome: BackgroundLaunchOutcome,
        notify_cleanup: impl FnOnce(BackgroundCleanupOutcome) + Send + 'static,
    ) -> Result<AcceptedLaunchOutcome, SessionHostError> {
        self.0.accept_launch_outcome(outcome, notify_cleanup)
    }

    #[cfg(test)]
    pub(crate) fn complete_test_launch(
        &mut self,
        permit: SessionStartPermit,
        target: TargetSystem,
        request: ConnectRequest,
    ) -> TestLaunchOutcome {
        self.0.complete_test_launch(permit, target, request)
    }

    pub fn send_command(&self, command: SessionCommand) -> Result<(), SessionHostError> {
        self.0.send_command(command)
    }

    pub fn drain_session_events(&mut self) -> Vec<(SessionId, SessionEvent)> {
        self.0.drain_session_events()
    }

    pub fn drain_video_worker_events(&mut self) -> Vec<(SessionId, VideoWorkerEvent)> {
        self.0.drain_video_worker_events()
    }

    pub fn drain_video_admissions(&mut self) -> Vec<(SessionId, VideoStreamAdmission)> {
        self.0.drain_video_admissions()
    }

    pub fn active_protocol_id(&self) -> Option<ProtocolId> {
        self.0.active_protocol_id()
    }

    pub(crate) fn active_high_performance_session_id(&self) -> Option<SessionId> {
        self.0.active_high_performance_session_id()
    }

    pub fn confirm_video_presented(&self, token: &VideoFrameToken) -> Result<(), SessionHostError> {
        self.0.confirm_video_presented(token)
    }

    pub fn video_is_ready(&self, identity: VideoStreamIdentity, generation: u64) -> bool {
        self.0.video_is_ready(identity, generation)
    }

    pub fn video_decode_load_snapshot(
        &self,
        identity: VideoStreamIdentity,
        generation: u64,
    ) -> Option<VideoDecodeLoadSnapshot> {
        self.0.video_decode_load_snapshot(identity, generation)
    }

    pub fn drain_frame_updates(&mut self) -> Vec<SurfaceUpdate> {
        self.0.drain_frame_updates()
    }

    pub fn drain_frame_transactions(&mut self) -> Result<CompiledFrameDrain, FrameCompileFailure> {
        self.0.drain_frame_transactions()
    }

    pub fn retire_frame_presentation(&mut self, session_id: SessionId) -> bool {
        self.0.retire_frame_presentation(session_id)
    }

    pub(crate) fn active_session_id(&self) -> Option<SessionId> {
        self.0.active_session_id()
    }

    pub fn is_active(&self) -> bool {
        self.0.is_active()
    }

    pub fn begin_cleanup(
        &mut self,
        notify: impl FnOnce(BackgroundCleanupOutcome) + Send + 'static,
    ) -> Result<bool, BackgroundCleanupFailure> {
        self.0.begin_cleanup(notify)
    }

    pub fn accept_cleanup_outcome(
        &mut self,
        outcome: BackgroundCleanupOutcome,
    ) -> Result<CleanupComplete, SessionHostError> {
        self.0.accept_cleanup_outcome(outcome)
    }

    #[cfg(test)]
    pub(crate) fn attach_frame_test_mailbox(
        &mut self,
        session_id: SessionId,
        mailbox: Arc<std::sync::Mutex<frd_frame::FrameMailbox>>,
    ) {
        self.0.attach_frame_test_mailbox(session_id, mailbox);
    }

    #[cfg(test)]
    pub(crate) fn test_mailbox(&self) -> Arc<std::sync::Mutex<frd_frame::FrameMailbox>> {
        self.0.test_mailbox()
    }
}
