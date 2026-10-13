use ironrdp_graphics::color_conversion::{ycbcr_to_rgba, YCbCrBuffer};
#[test]
fn upstream_color_overflow_is_profile_dependent() {
    let upstream = std::panic::catch_unwind(|| {
        let mut out = [0; 4];
        ycbcr_to_rgba(
            YCbCrBuffer {
                y: &[32767],
                cb: &[0],
                cr: &[0],
            },
            &mut out,
        )
        .unwrap();
        out
    });
    if cfg!(debug_assertions) {
        assert!(upstream.is_err());
    } else {
        assert_eq!(upstream.unwrap(), [0, 0, 0, 255]);
    }
}
