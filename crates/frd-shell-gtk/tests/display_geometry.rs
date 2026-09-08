use frd_core::{PixelRect, PixelSize};
use frd_shell_gtk::display_geometry::from_application_pixels as convert;

#[test]
fn native_monitor_uses_exact_double_scale_including_fractional() {
    for (scale, width, height, milli) in [
        (1.0, 2560, 1440, 1000),
        (2.0, 5120, 2880, 2000),
        (1.25, 3200, 1800, 1250),
        (1.5, 3840, 2160, 1500),
    ] {
        let result = convert(2560, 1440, scale, 800, 600, scale, false).unwrap();
        assert_eq!(result.physical_size, PixelSize::new(width, height).unwrap());
        assert_eq!(
            (result.logical_width, result.logical_height),
            (2560.0, 1440.0)
        );
        assert_eq!(result.scale_factor_milli, milli);
    }
}
#[test]
fn native_mode_has_no_2560_or_4k_cap() {
    let result = convert(7680, 4320, 1.0, 4096, 2160, 1.0, true).unwrap();
    assert_eq!(result.physical_size, PixelSize::new(7680, 4320).unwrap());
    assert_eq!(result.content_area.width, 4096);
    assert!(result.fullscreen);
    assert_eq!(
        result.work_area,
        PixelRect {
            x: 0,
            y: 0,
            width: 7680,
            height: 4320
        }
    );
}
#[test]
fn content_is_window_surface_scaled_clipped_and_excludes_titlebar() {
    let result = convert(2560, 1440, 1.25, 800, 556, 1.5, false).unwrap();
    // 输入已从600高窗口移除44高标题栏；不能再使用整窗或ceil(scale)=2。
    assert_eq!(
        result.content_area,
        PixelRect {
            x: 0,
            y: 0,
            width: 1200,
            height: 834
        }
    );
    let clipped = convert(1920, 1080, 1.25, 3000, 2000, 2.0, false).unwrap();
    assert_eq!(clipped.content_area, clipped.work_area);
}
#[test]
fn fractional_pixels_round_once_without_ceil_scale_inflation() {
    let result = convert(2047, 1151, 1.25, 801, 557, 1.25, false).unwrap();
    assert_eq!(result.physical_size, PixelSize::new(2559, 1439).unwrap());
    assert_eq!(result.content_area.width, 1001);
    assert_eq!(result.content_area.height, 696);
}
#[test]
fn invalid_extents_scales_and_overflow_are_rejected() {
    for scale in [
        0.0,
        -1.0,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MAX,
        0.00001,
    ] {
        assert!(convert(1920, 1080, scale, 800, 600, 1.0, false).is_none());
    }
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX, 0.00001] {
        assert!(convert(1920, 1080, 1.0, 800, 600, scale, false).is_none());
    }
    for (mw, mh, cw, ch) in [
        (0, 1080, 800, 600),
        (-1, 1080, 800, 600),
        (1920, 0, 800, 600),
        (1920, 1080, 0, 600),
        (1920, 1080, 800, -1),
    ] {
        assert!(convert(mw, mh, 1.0, cw, ch, 1.0, false).is_none());
    }
    assert!(convert(i32::MAX, 1080, 3.0, 800, 600, 1.0, false).is_none());
    assert!(convert(1920, 1080, 1.0, i32::MAX, 600, 3.0, false).is_none());
    assert!(convert(1, 1, 4_294_967.296, 1, 1, 1.0, false).is_none());
}
