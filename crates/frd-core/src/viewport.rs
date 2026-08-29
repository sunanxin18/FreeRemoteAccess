use crate::{PixelPoint, PixelRect, PixelSize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentViewport {
    pub drawable: PixelSize,
    pub content: PixelRect,
    pub remote: PixelSize,
}

impl ContentViewport {
    pub fn fit(remote: PixelSize, drawable: PixelSize) -> Self {
        let drawable = PixelSize {
            width: drawable.width.max(1),
            height: drawable.height.max(1),
        };
        Self::fit_in(
            remote,
            drawable,
            PixelRect {
                x: 0,
                y: 0,
                width: drawable.width,
                height: drawable.height,
            },
        )
        .expect("完整 drawable 必须形成有效视口")
    }

    pub fn fit_in(remote: PixelSize, drawable: PixelSize, bounds: PixelRect) -> Option<Self> {
        let remote = PixelSize {
            width: remote.width.max(1),
            height: remote.height.max(1),
        };
        if drawable.width == 0
            || drawable.height == 0
            || bounds.width == 0
            || bounds.height == 0
            || bounds.x.checked_add(bounds.width)? > drawable.width
            || bounds.y.checked_add(bounds.height)? > drawable.height
        {
            return None;
        }

        let drawable_by_remote_height = u128::from(bounds.width) * u128::from(remote.height);
        let drawable_height_by_remote_width = u128::from(bounds.height) * u128::from(remote.width);
        let (width, height) = if drawable_by_remote_height <= drawable_height_by_remote_width {
            (
                bounds.width,
                ((u128::from(remote.height) * u128::from(bounds.width)) / u128::from(remote.width))
                    .clamp(1, u128::from(bounds.height)) as u32,
            )
        } else {
            (
                ((u128::from(remote.width) * u128::from(bounds.height)) / u128::from(remote.height))
                    .clamp(1, u128::from(bounds.width)) as u32,
                bounds.height,
            )
        };

        Some(Self {
            drawable,
            content: PixelRect {
                x: bounds.x + (bounds.width - width) / 2,
                y: bounds.y + (bounds.height - height) / 2,
                width,
                height,
            },
            remote,
        })
    }

    pub fn map_pointer(self, drawable_x: f32, drawable_y: f32) -> Option<PixelPoint> {
        fn map_axis(value: f32, origin: u32, extent: u32, remote_extent: u32) -> Option<u32> {
            if !value.is_finite() || extent == 0 || remote_extent == 0 {
                return None;
            }

            let start = origin as f32;
            let end = origin.checked_add(extent)? as f32;
            if value < start || value >= end {
                return None;
            }

            if extent == 1 || remote_extent == 1 {
                return Some(0);
            }

            let scaled = ((value - start) as f64 * f64::from(remote_extent - 1)
                / f64::from(extent - 1))
            .floor() as u32;
            Some(scaled.min(remote_extent - 1))
        }

        Some(PixelPoint {
            x: map_axis(
                drawable_x,
                self.content.x,
                self.content.width,
                self.remote.width,
            )?,
            y: map_axis(
                drawable_y,
                self.content.y,
                self.content.height,
                self.remote.height,
            )?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ContentViewport;
    use crate::{PixelPoint, PixelSize};

    #[test]
    fn landscape_remote_maps_center_and_rejects_letterbox() {
        let viewport = ContentViewport::fit(
            PixelSize {
                width: 2560,
                height: 1440,
            },
            PixelSize {
                width: 1280,
                height: 720,
            },
        );

        assert_eq!(
            viewport.map_pointer(640.0, 360.0),
            Some(PixelPoint { x: 1280, y: 720 })
        );
        assert_eq!(
            viewport.map_pointer(1279.0, 719.0),
            Some(PixelPoint { x: 2559, y: 1439 })
        );
        assert_eq!(viewport.map_pointer(-0.1, 0.0), None);

        let letterboxed = ContentViewport::fit(
            PixelSize {
                width: 2560,
                height: 1440,
            },
            PixelSize {
                width: 720,
                height: 1280,
            },
        );
        assert_eq!(letterboxed.content.y, 437);
        assert_eq!(letterboxed.map_pointer(0.0, 0.0), None);
    }

    #[test]
    fn portrait_remote_centers_content_and_rejects_side_bars() {
        let viewport = ContentViewport::fit(
            PixelSize {
                width: 1440,
                height: 2560,
            },
            PixelSize {
                width: 1280,
                height: 720,
            },
        );

        assert_eq!(viewport.content.x, 437);
        assert_eq!(viewport.content.width, 405);
        assert_eq!(
            viewport.map_pointer(437.0, 0.0),
            Some(PixelPoint { x: 0, y: 0 })
        );
        assert_eq!(viewport.map_pointer(0.0, 0.0), None);
        assert_eq!(
            viewport.map_pointer(841.999, 719.999),
            Some(PixelPoint { x: 1439, y: 2559 })
        );
    }

    #[test]
    fn square_remote_centers_content_and_clamps_valid_bottom_right() {
        let viewport = ContentViewport::fit(
            PixelSize {
                width: 100,
                height: 100,
            },
            PixelSize {
                width: 300,
                height: 200,
            },
        );

        assert_eq!(viewport.content.x, 50);
        assert_eq!(viewport.content.y, 0);
        assert_eq!(viewport.content.width, 200);
        assert_eq!(viewport.content.height, 200);
        assert_eq!(
            viewport.map_pointer(249.999, 199.999),
            Some(PixelPoint { x: 99, y: 99 })
        );
        assert_eq!(viewport.map_pointer(250.0, 199.0), None);
    }

    #[test]
    fn one_pixel_axes_map_safely_without_division_by_zero() {
        let viewport = ContentViewport::fit(
            PixelSize {
                width: 1,
                height: 5,
            },
            PixelSize {
                width: 1,
                height: 5,
            },
        );

        assert_eq!(
            viewport.map_pointer(0.0, 4.0),
            Some(PixelPoint { x: 0, y: 4 })
        );
        assert_eq!(viewport.map_pointer(1.0, 4.0), None);
    }

    #[test]
    fn inset_drawable_keeps_remote_pixels_and_pointer_mapping_below_toolbar() {
        let viewport = ContentViewport::fit_in(
            PixelSize {
                width: 100,
                height: 100,
            },
            PixelSize {
                width: 300,
                height: 240,
            },
            crate::PixelRect {
                x: 0,
                y: 40,
                width: 300,
                height: 200,
            },
        )
        .expect("工具栏下方区域有效");

        assert_eq!(
            viewport.content,
            crate::PixelRect {
                x: 50,
                y: 40,
                width: 200,
                height: 200,
            }
        );
        assert_eq!(
            viewport.map_pointer(50.0, 40.0),
            Some(PixelPoint { x: 0, y: 0 })
        );
        assert_eq!(viewport.map_pointer(50.0, 39.999), None);
    }
}
