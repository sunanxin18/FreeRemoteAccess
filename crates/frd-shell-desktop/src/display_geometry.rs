//! 将 winit 窗口/显示器状态转换为协议无关的物理显示几何。

use frd_core::{DisplayGeometry, PixelRect, PixelSize};
use winit::window::Window;

use crate::LogicalWindowExtent;

/// 使用当前显示器的物理尺寸，并把窗口的远程内容矩形保留为物理像素。
///
/// winit 没有为所有桌面平台提供统一的工作区 API，因此工作区在这里
/// 使用显示器物理矩形作为保守回退；平台 shell 可以在获得原生工作区
/// 信息后替换该矩形，而协议层无需变化。
pub fn from_window(window: &Window, content_area: Option<PixelRect>) -> Option<DisplayGeometry> {
    let monitor = window.current_monitor()?;
    let monitor_size = monitor.size();
    let physical_size = PixelSize::new(monitor_size.width, monitor_size.height)?;
    let scale_factor = window.scale_factor();
    let scale_factor_milli = scale_factor_to_milli(scale_factor)?;
    let logical_width = f64::from(physical_size.width) / scale_factor;
    let logical_height = f64::from(physical_size.height) / scale_factor;
    let full_area = PixelRect {
        x: 0,
        y: 0,
        width: physical_size.width,
        height: physical_size.height,
    };
    let content_area = content_area
        .filter(|area| {
            area.checked_bounds().is_some_and(|(_, end)| {
                end.x <= physical_size.width && end.y <= physical_size.height
            })
        })
        .unwrap_or_else(|| {
            let size = window.inner_size();
            PixelRect {
                x: 0,
                y: 0,
                width: size.width.min(physical_size.width),
                height: size.height.min(physical_size.height),
            }
        });
    DisplayGeometry::new(
        logical_width,
        logical_height,
        physical_size,
        full_area,
        content_area,
        scale_factor_milli,
        window.fullscreen().is_some(),
    )
}

pub fn scale_factor_to_milli(scale_factor: f64) -> Option<u32> {
    if !scale_factor.is_finite() || scale_factor <= 0.0 {
        return None;
    }
    let scaled = (scale_factor * 1000.0).round();
    (scaled >= 1.0 && scaled <= f64::from(u32::MAX)).then_some(scaled as u32)
}

pub fn logical_extent_from_physical(
    physical_size: PixelSize,
    scale_factor: f64,
) -> Option<LogicalWindowExtent> {
    if !scale_factor.is_finite() || scale_factor <= 0.0 {
        return None;
    }
    let width = f64::from(physical_size.width) / scale_factor;
    let height = f64::from(physical_size.height) / scale_factor;
    (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
        .then_some(LogicalWindowExtent::new(width, height))
}

#[cfg(test)]
mod tests {
    use super::scale_factor_to_milli;
    use crate::LogicalWindowExtent;
    use frd_core::PixelSize;

    #[test]
    fn scale_factor_conversion_preserves_retina_precision() {
        assert_eq!(scale_factor_to_milli(1.0), Some(1000));
        assert_eq!(scale_factor_to_milli(2.0), Some(2000));
        assert_eq!(scale_factor_to_milli(1.25), Some(1250));
    }

    #[test]
    fn invalid_or_overflowing_scale_factors_are_rejected() {
        assert_eq!(scale_factor_to_milli(0.0), None);
        assert_eq!(scale_factor_to_milli(-1.0), None);
        assert_eq!(scale_factor_to_milli(f64::NAN), None);
        assert_eq!(scale_factor_to_milli(f64::INFINITY), None);
    }

    #[test]
    fn retina_physical_monitor_becomes_logical_window_extent() {
        let extent =
            super::logical_extent_from_physical(PixelSize::new(3456, 2234).unwrap(), 2.0).unwrap();
        assert_eq!(extent, LogicalWindowExtent::new(1728.0, 1117.0));
    }
}
