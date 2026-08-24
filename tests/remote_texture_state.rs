use freeremotedesk::core::{FrameRect, RemotePixelFormat, RenderUpdate};
use freeremotedesk::ui::{
    GpuFailureLatch, QueuedSessionProgress, RemoteTextureAction, RemoteTextureState,
    RendererRuntimePolicy, ResetDisposition, SurfaceAcquireOutcome, SurfaceRecoveryPlan,
    SurfaceRecoveryStep, TextureUpdateDisposition,
};

#[test]
fn reset_changes_texture_only_for_a_newer_generation() {
    let mut state = RemoteTextureState::empty();

    assert_eq!(
        state.apply_reset(4, 1280, 720).unwrap(),
        ResetDisposition::Created
    );
    assert_eq!(
        state.apply_reset(3, 1920, 1080).unwrap(),
        ResetDisposition::Stale
    );
    assert_eq!(state.dimensions(), Some((1280, 720)));
}

#[test]
fn surface_loss_does_not_change_remote_generation() {
    let mut state = RemoteTextureState::fixture(9, 800, 600);

    state.on_surface_lost();

    assert_eq!(state.generation(), Some(9));
    assert!(!state.surface_available());
}

#[test]
fn dirty_rect_must_fit_current_generation_and_surface() {
    let state = RemoteTextureState::fixture(2, 8, 8);
    let rect = FrameRect::new(6, 6, 3, 2).unwrap();
    let update = RenderUpdate::dirty_rect(
        2,
        rect,
        RemotePixelFormat::Bgra8Srgb,
        12,
        vec![0; 24].into_boxed_slice(),
    )
    .unwrap();

    let error = state.classify(&update).unwrap_err();

    assert_eq!(error.code(), "texture_rect_out_of_bounds");
}

#[test]
fn stale_present_is_ignored_before_gpu_submission() {
    let state = RemoteTextureState::fixture(5, 8, 8);

    assert_eq!(
        state.classify(&RenderUpdate::present(4)).unwrap(),
        TextureUpdateDisposition::Stale
    );
}

#[test]
fn runtime_surface_recovery_preserves_generation_bound_remote_texture() {
    let mut policy = RendererRuntimePolicy::new();
    assert_eq!(
        policy.apply_reset(7, 1280, 720).unwrap(),
        ResetDisposition::Created
    );

    assert_eq!(
        policy.on_surface_acquire(SurfaceAcquireOutcome::Lost),
        SurfaceRecoveryPlan::Recover(&[
            SurfaceRecoveryStep::RecreateSurface,
            SurfaceRecoveryStep::ReconfigureExistingSurface,
            SurfaceRecoveryStep::RequestRedraw,
        ])
    );
    assert_eq!(policy.generation(), Some(7));
    assert_eq!(policy.dimensions(), Some((1280, 720)));
    assert!(!policy.surface_available());

    assert_eq!(
        policy.on_surface_acquire(SurfaceAcquireOutcome::Outdated),
        SurfaceRecoveryPlan::Recover(&[
            SurfaceRecoveryStep::ReconfigureExistingSurface,
            SurfaceRecoveryStep::RequestRedraw,
        ])
    );
    assert_eq!(policy.generation(), Some(7));
    assert_eq!(policy.dimensions(), Some((1280, 720)));
    assert!(!policy.surface_available());
}

#[test]
fn authenticated_session_lifecycle_clears_remote_texture_before_generation_one_reconnect() {
    let mut policy = RendererRuntimePolicy::new();

    assert_eq!(
        policy.begin_authenticated_session(),
        RemoteTextureAction::Clear
    );
    assert_eq!(
        policy.apply_reset(7, 1280, 720).unwrap(),
        ResetDisposition::Created
    );
    assert_eq!(
        policy.finish_disconnected_session(),
        RemoteTextureAction::Clear
    );
    assert_eq!(policy.generation(), None);

    assert_eq!(
        policy.begin_authenticated_session(),
        RemoteTextureAction::Clear
    );
    assert_eq!(
        policy.apply_reset(1, 1024, 768).unwrap(),
        ResetDisposition::Created
    );
    assert_eq!(policy.generation(), Some(1));

    assert_eq!(policy.finish_failed_session(), RemoteTextureAction::Clear);
    assert_eq!(policy.generation(), None);
    assert_eq!(
        policy.on_surface_acquire(SurfaceAcquireOutcome::Validation),
        SurfaceRecoveryPlan::FailSession
    );
}

#[test]
fn production_surface_recovery_plan_orders_every_wgpu_acquire_outcome() {
    let mut policy = RendererRuntimePolicy::new();
    assert_eq!(
        policy.apply_reset(7, 1280, 720).unwrap(),
        ResetDisposition::Created
    );
    let texture_identity = policy.remote_texture_identity();

    assert_eq!(
        policy.on_surface_acquire(SurfaceAcquireOutcome::Success),
        SurfaceRecoveryPlan::Render
    );
    assert_eq!(
        policy.on_surface_acquire(SurfaceAcquireOutcome::Suboptimal),
        SurfaceRecoveryPlan::RenderThen(&[
            SurfaceRecoveryStep::PresentFrame,
            SurfaceRecoveryStep::ReconfigureExistingSurface,
            SurfaceRecoveryStep::RequestRedraw,
        ])
    );
    assert_eq!(
        policy.on_surface_acquire(SurfaceAcquireOutcome::Timeout),
        SurfaceRecoveryPlan::SkipUntilNextWake
    );
    assert_eq!(
        policy.on_surface_acquire(SurfaceAcquireOutcome::Occluded),
        SurfaceRecoveryPlan::WaitForVisibility
    );
    assert_eq!(
        policy.on_surface_acquire(SurfaceAcquireOutcome::Outdated),
        SurfaceRecoveryPlan::Recover(&[
            SurfaceRecoveryStep::ReconfigureExistingSurface,
            SurfaceRecoveryStep::RequestRedraw,
        ])
    );
    assert_eq!(
        policy.on_surface_acquire(SurfaceAcquireOutcome::Lost),
        SurfaceRecoveryPlan::Recover(&[
            SurfaceRecoveryStep::RecreateSurface,
            SurfaceRecoveryStep::ReconfigureExistingSurface,
            SurfaceRecoveryStep::RequestRedraw,
        ])
    );
    assert_eq!(
        policy.on_surface_acquire(SurfaceAcquireOutcome::Validation),
        SurfaceRecoveryPlan::FailSession
    );
    assert_eq!(policy.generation(), Some(7));
    assert_eq!(policy.remote_texture_identity(), texture_identity);
}

#[test]
fn gpu_failure_latch_blocks_queued_session_progress_and_input_until_worker_completion() {
    let mut latch = GpuFailureLatch::default();

    assert!(latch.latch("surface_validation_failed"));
    assert!(!latch.latch("different_gpu_failure"));
    assert!(latch.blocks_session_progress());
    assert!(latch.blocks_remote_input());
    assert!(!latch.admits_queued_progress(QueuedSessionProgress::Render));
    assert!(!latch.admits_queued_progress(QueuedSessionProgress::SurfaceReset));
    assert!(!latch.admits_queued_progress(QueuedSessionProgress::Connected));
    assert_eq!(latch.first_error_code(), Some("surface_validation_failed"));

    assert_eq!(
        latch.release_after_worker_completion(),
        Some("surface_validation_failed")
    );
    assert!(!latch.blocks_session_progress());
    assert!(!latch.blocks_remote_input());
    assert!(latch.admits_queued_progress(QueuedSessionProgress::Render));
}
