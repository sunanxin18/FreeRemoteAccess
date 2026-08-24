use freeremotedesk::core::{FrameRect, RemotePixelFormat, RenderUpdate};
use freeremotedesk::ui::{RemoteTextureState, ResetDisposition, TextureUpdateDisposition};

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
