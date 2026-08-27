use crate::{PixelPoint, PixelRect, PixelSize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentViewport {
    pub drawable: PixelSize,
    pub content: PixelRect,
    pub remote: PixelSize,
}

impl ContentViewport {
    pub fn fit(remote: PixelSize, drawable: PixelSize) -> Self {
        let remote = PixelSize {
            width: remote.width.max(1),
            height: remote.height.max(1),
        };
        let drawable = PixelSize {
            width: drawable.width.max(1),
            height: drawable.height.max(1),
        };

        let drawable_by_remote_height = u128::from(drawable.width) * u128::from(remote.height);
        let drawable_height_by_remote_width =
            u128::from(drawable.height) * u128::from(remote.width);
        let (width, height) = if drawable_by_remote_height <= drawable_height_by_remote_width {
            (
                drawable.width,
                ((u128::from(remote.height) * u128::from(drawable.width))
                    / u128::from(remote.width))
                .clamp(1, u128::from(drawable.height)) as u32,
            )
        } else {
            (
                ((u128::from(remote.width) * u128::from(drawable.height))
                    / u128::from(remote.height))
                .clamp(1, u128::from(drawable.width)) as u32,
                drawable.height,
            )
        };

        Self {
            drawable,
            content: PixelRect {
                x: (drawable.width - width) / 2,
                y: (drawable.height - height) / 2,
                width,
                height,
            },
            remote,
        }
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

            let scaled = ((value - start) as f64 * f64::from(remote_extent) / f64::from(extent))
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
            Some(PixelPoint { x: 2558, y: 1438 })
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
}
