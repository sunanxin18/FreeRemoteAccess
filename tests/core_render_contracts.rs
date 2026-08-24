use freeremotedesk::core::frame::{
    FrameRect, GenerationDisposition, RemotePixelFormat, RemoteSurfaceState, RenderUpdate,
};
use freeremotedesk::core::viewport::RemoteViewportTransform;

#[test]
fn dirty_rect_rejects_bytes_shorter_than_stride_times_height() {
    let error = RenderUpdate::dirty_rect(
        7,
        FrameRect::new(0, 0, 2, 2).unwrap(),
        RemotePixelFormat::Bgra8Srgb,
        8,
        vec![0; 15].into_boxed_slice(),
    )
    .unwrap_err();

    assert_eq!(error.code(), "dirty_rect_length_mismatch");
}

#[test]
fn dirty_rect_rejects_a_stride_smaller_than_one_pixel_row() {
    let error = RenderUpdate::dirty_rect(
        7,
        FrameRect::new(0, 0, 3, 1).unwrap(),
        RemotePixelFormat::Rgba8Srgb,
        8,
        vec![0; 8].into_boxed_slice(),
    )
    .unwrap_err();

    assert_eq!(error.code(), "dirty_rect_stride_too_small");
}

#[test]
fn stale_generation_is_classified_before_upload() {
    let state = RemoteSurfaceState::new(8, 1920, 1080).unwrap();

    assert_eq!(state.classify_generation(7), GenerationDisposition::Stale);
    assert_eq!(state.classify_generation(8), GenerationDisposition::Current);
    assert_eq!(state.classify_generation(9), GenerationDisposition::Future);
}

#[test]
fn surface_rejects_more_than_sixty_four_million_pixels() {
    let error = RemoteSurfaceState::new(1, 16_000, 8_000).unwrap_err();

    assert_eq!(error.code(), "surface_dimensions_too_large");
}

#[test]
fn square_host_letterboxes_wide_remote_and_maps_center() {
    let transform = RemoteViewportTransform::new((1000, 1000), (1920, 1080), 1.0).unwrap();

    assert_eq!(transform.remote_point((500.0, 500.0)), Some((960, 540)));
    assert_eq!(transform.remote_point((500.0, 100.0)), None);
}

#[test]
fn viewport_rejects_non_finite_scale() {
    let error = RemoteViewportTransform::new((1000, 1000), (1920, 1080), f64::NAN).unwrap_err();

    assert_eq!(error.code(), "viewport_scale_invalid");
}
