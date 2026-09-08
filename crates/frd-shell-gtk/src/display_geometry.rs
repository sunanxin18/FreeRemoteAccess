//! GTK显示器/窗口几何到协议无关物理像素的转换。
//!
//! 固定依据 GTK 4.14.5 gdk/gdkmonitor.c 的 get_geometry/get_scale，及
//! gdk/gdksurface.c 的 get_scale（since 4.12）。Monitor geometry 是application pixels；
//! Monitor::scale()是double，不能用向上取整的scale_factor替代。
//! https://github.com/GNOME/gtk/blob/4.14.5/gdk/gdkmonitor.c
//!
//! 这里计算远程分辨率意图，不计算GL framebuffer。GLArea的实际drawable仍须使用
//! widget allocation × widget整数scale，与窗口合成后的物理内容尺寸分开。
use frd_core::{DisplayGeometry, PixelRect, PixelSize};

fn rounded_positive_u32(value: f64) -> Option<u32> {
    let rounded = value.round();
    (value.is_finite() && rounded >= 1.0 && rounded <= f64::from(u32::MAX))
        .then_some(rounded as u32)
}
fn scaled_extent(width: i32, height: i32, scale: f64) -> Option<PixelSize> {
    if width <= 0 || height <= 0 || !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    PixelSize::new(
        rounded_positive_u32(f64::from(width) * scale)?,
        rounded_positive_u32(f64::from(height) * scale)?,
    )
}

/// content_width/height必须是远程内容widget的application-pixel allocation，不能传
/// 含标题栏/应用工具栏的整窗尺寸。content_scale是该窗口surface的double scale。
/// 返回content_area以内容自身为原点，仅保存可请求的尺寸，不把窗口在桌面的位置
/// 或标题栏偏移带入协议；跨屏/超大窗口的尺寸裁剪到当前显示器物理范围。
/// 没有统一的GTK工作区API时，work_area明确回退到完整显示器。
#[allow(clippy::too_many_arguments)]
pub fn from_application_pixels(
    monitor_width: i32,
    monitor_height: i32,
    monitor_scale: f64,
    content_width: i32,
    content_height: i32,
    content_scale: f64,
    fullscreen: bool,
) -> Option<DisplayGeometry> {
    let physical_size = scaled_extent(monitor_width, monitor_height, monitor_scale)?;
    let content_size = scaled_extent(content_width, content_height, content_scale)?;
    let scale_factor_milli = rounded_positive_u32(monitor_scale * 1000.0)?;
    DisplayGeometry::new(
        f64::from(monitor_width),
        f64::from(monitor_height),
        physical_size,
        PixelRect {
            x: 0,
            y: 0,
            width: physical_size.width,
            height: physical_size.height,
        },
        PixelRect {
            x: 0,
            y: 0,
            width: content_size.width.min(physical_size.width),
            height: content_size.height.min(physical_size.height),
        },
        scale_factor_milli,
        fullscreen,
    )
}

/// 采集当前窗口所在显示器。remote_content必须是窗口内容树中的远程区域，排除标题栏。
#[cfg(all(target_os = "linux", feature = "gtk-shell"))]
pub fn from_window(
    window: &gtk4::Window,
    remote_content: &impl gtk4::prelude::IsA<gtk4::Widget>,
) -> Option<DisplayGeometry> {
    use gtk4::prelude::*;
    let remote_content = remote_content.as_ref();
    // Window::child不含native titlebar；允许该child本身或其后代，不接受window/header。
    let child = window.child()?;
    if *remote_content != child && !remote_content.is_ancestor(&child) {
        return None;
    }
    let surface = window.surface()?;
    if remote_content
        .native()
        .and_then(|native| native.surface())
        .as_ref()
        != Some(&surface)
    {
        return None;
    }
    let monitor = WidgetExt::display(window).monitor_at_surface(&surface)?;
    if !monitor.is_valid() {
        return None;
    }
    let geometry = monitor.geometry();
    from_application_pixels(
        geometry.width(),
        geometry.height(),
        monitor.scale(),
        remote_content.width(),
        remote_content.height(),
        surface.scale(),
        window.is_fullscreen(),
    )
}
