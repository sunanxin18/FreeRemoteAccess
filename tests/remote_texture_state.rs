use freeremotedesk::core::{FrameRect, RemotePixelFormat, RenderUpdate};
use freeremotedesk::ui::{
    GpuFailureGate, RecoveryExecution, RemoteTextureAction, RemoteTextureState,
    RendererFailureAction, RendererRuntimePolicy, ResetDisposition, SurfaceAcquireOutcome,
    SurfaceRecoveryController, SurfaceRecoveryExecutor, SurfaceRecoveryPort,
    TextureUpdateDisposition,
};
use std::time::Duration;
use std::{cell::RefCell, rc::Rc};

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

    assert_eq!(policy.generation(), Some(7));
    assert_eq!(policy.dimensions(), Some((1280, 720)));
    assert!(!policy.surface_available());

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
}

#[test]
fn production_recovery_executor_presents_before_reconfigure_and_preserves_remote_slot() {
    #[derive(Default)]
    struct FakePort {
        operations: Rc<RefCell<Vec<&'static str>>>,
        remote_slot: u64,
    }

    impl SurfaceRecoveryPort for FakePort {
        type Error = &'static str;

        fn recreate_surface(&mut self) -> Result<(), &'static str> {
            self.operations.borrow_mut().push("recreate");
            Ok(())
        }

        fn configure_surface(&mut self) -> Result<(), &'static str> {
            self.operations.borrow_mut().push("configure");
            Ok(())
        }
    }

    let mut controller = SurfaceRecoveryController::default();
    let executor = SurfaceRecoveryExecutor;
    let mut port = FakePort {
        remote_slot: 41,
        ..Default::default()
    };
    let remote_slot = port.remote_slot;
    let operations = port.operations.clone();

    let decision = controller.on_acquire(SurfaceAcquireOutcome::Suboptimal);
    assert_eq!(
        executor
            .execute_post_present(
                decision,
                || operations.borrow_mut().push("present"),
                &mut port
            )
            .unwrap(),
        RecoveryExecution::RetryAfter(Duration::ZERO)
    );
    assert_eq!(*port.operations.borrow(), ["present", "configure"]);
    assert_eq!(port.remote_slot, remote_slot);

    let decision = controller.on_acquire(SurfaceAcquireOutcome::Lost);
    assert_eq!(
        executor.execute_without_frame(decision, &mut port).unwrap(),
        RecoveryExecution::RetryAfter(Duration::from_millis(50))
    );
    assert_eq!(
        *port.operations.borrow(),
        ["present", "configure", "recreate", "configure"]
    );
    assert_eq!(port.remote_slot, remote_slot);
}

#[test]
fn production_recovery_executor_propagates_local_surface_failures() {
    struct FailingPort;

    impl SurfaceRecoveryPort for FailingPort {
        type Error = &'static str;

        fn recreate_surface(&mut self) -> Result<(), Self::Error> {
            Err("surface_recreate_failed")
        }

        fn configure_surface(&mut self) -> Result<(), Self::Error> {
            Err("surface_config_failed")
        }
    }

    let executor = SurfaceRecoveryExecutor;
    let mut port = FailingPort;
    assert_eq!(
        executor.execute_without_frame(
            freeremotedesk::ui::SurfaceRecoveryDecision::RecreateThenConfigureThenRetry(
                Duration::ZERO
            ),
            &mut port
        ),
        Err("surface_recreate_failed")
    );
    assert_eq!(
        executor.execute_post_present(
            freeremotedesk::ui::SurfaceRecoveryDecision::RenderThenReconfigure(Duration::ZERO),
            || {},
            &mut port
        ),
        Err("surface_config_failed")
    );
}

#[test]
fn recovery_backoff_and_failure_gate_are_bounded_and_fail_closed() {
    let mut controller = SurfaceRecoveryController::default();
    assert_eq!(
        controller.on_acquire(SurfaceAcquireOutcome::Success),
        freeremotedesk::ui::SurfaceRecoveryDecision::Render
    );
    assert_eq!(
        controller.on_acquire(SurfaceAcquireOutcome::Suboptimal),
        freeremotedesk::ui::SurfaceRecoveryDecision::RenderThenReconfigure(Duration::ZERO)
    );
    assert_eq!(
        controller.on_acquire(SurfaceAcquireOutcome::Success),
        freeremotedesk::ui::SurfaceRecoveryDecision::Render
    );
    assert_eq!(
        controller.on_acquire(SurfaceAcquireOutcome::Occluded),
        freeremotedesk::ui::SurfaceRecoveryDecision::WaitForVisibility
    );
    assert_eq!(
        controller.on_acquire(SurfaceAcquireOutcome::Validation),
        freeremotedesk::ui::SurfaceRecoveryDecision::Terminal("surface_validation_failed")
    );

    let mut controller = SurfaceRecoveryController::default();
    assert_eq!(
        controller.on_acquire(SurfaceAcquireOutcome::Timeout),
        freeremotedesk::ui::SurfaceRecoveryDecision::RetryAfter(Duration::from_millis(25))
    );
    assert_eq!(
        controller.on_acquire(SurfaceAcquireOutcome::Outdated),
        freeremotedesk::ui::SurfaceRecoveryDecision::ReconfigureThenRetry(Duration::from_millis(
            50
        ))
    );
    assert_eq!(
        controller.on_acquire(SurfaceAcquireOutcome::Lost),
        freeremotedesk::ui::SurfaceRecoveryDecision::RecreateThenConfigureThenRetry(
            Duration::from_millis(100)
        )
    );
    assert_eq!(
        controller.on_acquire(SurfaceAcquireOutcome::Lost),
        freeremotedesk::ui::SurfaceRecoveryDecision::RecreateThenConfigureThenRetry(
            Duration::from_millis(200)
        )
    );
    assert_eq!(
        controller.on_acquire(SurfaceAcquireOutcome::Lost),
        freeremotedesk::ui::SurfaceRecoveryDecision::Terminal("surface_recovery_exhausted")
    );

    let mut gate = GpuFailureGate::default();
    assert!(gate.latch("surface_recreate_failed"));
    assert!(!gate.latch("surface_format_changed"));
    assert!(!gate.permits_new_session());
    assert!(gate.blocks_remote_input());
    assert_eq!(gate.first_error_code(), Some("surface_recreate_failed"));
    assert!(!gate.release_after_worker_completion(false));
    assert!(!gate.permits_new_session());
    assert!(gate.release_after_worker_completion(true));
    assert!(gate.permits_new_session());

    assert_eq!(
        gate.on_terminal_renderer_error("surface_config_failed", true),
        RendererFailureAction::OrderlyDisconnect
    );
    assert_eq!(
        gate.on_terminal_renderer_error("surface_recreate_failed", true),
        RendererFailureAction::OrderlyDisconnect
    );
    assert_eq!(gate.first_error_code(), Some("surface_config_failed"));
    assert!(!gate.release_after_worker_completion(false));
    assert!(gate.release_after_worker_completion(true));

    assert_eq!(
        gate.on_terminal_renderer_error("surface_capabilities_failed", false),
        RendererFailureAction::ExitFailClosed
    );
    assert!(!gate.permits_new_session());
    assert_eq!(gate.first_error_code(), Some("surface_capabilities_failed"));
}
