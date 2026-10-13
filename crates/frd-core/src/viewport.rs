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

/// 已验证的带裁剪仿射视口；输入与绘制共享同一个逆变换。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformedViewport {
    base: ContentViewport,
    clip: PixelRect,
    origin: [f32; 2],
    extent: [f32; 2],
}
impl TransformedViewport {
    pub fn new(base: ContentViewport, clip: PixelRect, scale: f32, pan: [f32; 2]) -> Option<Self> {
        ContentViewport::fit_in(base.remote, base.drawable, clip)?;
        ContentViewport::fit_in(base.remote, base.drawable, base.content)?;
        if base.remote.width == 0
            || base.remote.height == 0
            || !scale.is_finite()
            || scale <= 0.0
            || !pan.iter().all(|v| v.is_finite())
        {
            return None;
        }
        let extent = [
            base.content.width as f32 * scale,
            base.content.height as f32 * scale,
        ];
        let origin = [
            base.content.x as f32 + (base.content.width as f32 - extent[0]) / 2.0 + pan[0],
            base.content.y as f32 + (base.content.height as f32 - extent[1]) / 2.0 + pan[1],
        ];
        if !extent
            .iter()
            .all(|v| v.is_finite() && *v >= 1.0 && *v <= 16777216.0)
            || !origin
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 16777216.0)
        {
            return None;
        }
        if origin[0] >= (clip.x + clip.width) as f32
            || origin[1] >= (clip.y + clip.height) as f32
            || origin[0] + extent[0] <= clip.x as f32
            || origin[1] + extent[1] <= clip.y as f32
        {
            return None;
        }
        Some(Self {
            base,
            clip,
            origin,
            extent,
        })
    }
    pub fn base(self) -> ContentViewport {
        self.base
    }
    pub fn clip(self) -> PixelRect {
        self.clip
    }
    pub fn origin(self) -> [f32; 2] {
        self.origin
    }
    pub fn extent(self) -> [f32; 2] {
        self.extent
    }
    pub fn map_pointer(self, x: f32, y: f32) -> Option<PixelPoint> {
        if !x.is_finite()
            || !y.is_finite()
            || x < self.clip.x as f32
            || y < self.clip.y as f32
            || x >= (self.clip.x + self.clip.width) as f32
            || y >= (self.clip.y + self.clip.height) as f32
            || x < self.origin[0]
            || y < self.origin[1]
            || x >= self.origin[0] + self.extent[0]
            || y >= self.origin[1] + self.extent[1]
        {
            return None;
        }
        let axis = |v: f32, i: usize, n: u32| {
            if self.extent[i] <= 1.0 || n <= 1 {
                return 0;
            }
            ((v - self.origin[i]) as f64 * f64::from(n - 1) / f64::from(self.extent[i] - 1.0))
                .floor()
                .min(f64::from(n - 1)) as u32
        };
        Some(PixelPoint {
            x: axis(x, 0, self.base.remote.width),
            y: axis(y, 1, self.base.remote.height),
        })
    }
    /// 有界四边形的 NDC xy 与纹理 uv；GPU 裁剪允许负 origin。
    /// 不使用越过远端矩形的超大三角形，以免 clamp 采样填满 letterbox。
    pub fn vertices(self) -> [[f32; 4]; 6] {
        let [x, y] = self.origin;
        let [w, h] = self.extent;
        let dw = self.base.drawable.width as f32;
        let dh = self.base.drawable.height as f32;
        let tl = [2.0 * x / dw - 1.0, 1.0 - 2.0 * y / dh, 0.0, 0.0];
        let tr = [2.0 * (x + w) / dw - 1.0, 1.0 - 2.0 * y / dh, 1.0, 0.0];
        let bl = [2.0 * x / dw - 1.0, 1.0 - 2.0 * (y + h) / dh, 0.0, 1.0];
        let br = [2.0 * (x + w) / dw - 1.0, 1.0 - 2.0 * (y + h) / dh, 1.0, 1.0];
        [bl, br, tl, tl, br, tr]
    }
}

#[cfg(test)]
mod transform_tests {
    use super::*;
    #[test]
    fn identity_and_fractional_negative_pan_share_clip_and_inverse() {
        let size = PixelSize {
            width: 100,
            height: 100,
        };
        let base = ContentViewport::fit(size, size);
        let identity = TransformedViewport::new(base, base.content, 1.0, [0.0; 2]).unwrap();
        for (x, y) in [(0.0, 0.0), (49.25, 68.5), (99.9, 99.9)] {
            assert_eq!(identity.map_pointer(x, y), base.map_pointer(x, y));
        }
        let narrow = ContentViewport::fit(
            PixelSize {
                width: 100,
                height: 100,
            },
            PixelSize {
                width: 1,
                height: 1,
            },
        );
        let narrow_transform =
            TransformedViewport::new(narrow, narrow.content, 1.0, [0.0; 2]).unwrap();
        assert_eq!(
            narrow_transform.map_pointer(0.9, 0.9),
            narrow.map_pointer(0.9, 0.9)
        );
        assert!(TransformedViewport::new(
            ContentViewport {
                content: PixelRect {
                    x: 99,
                    y: 0,
                    width: 100,
                    height: 100
                },
                ..base
            },
            base.content,
            1.0,
            [0.0; 2]
        )
        .is_none());
        assert!(TransformedViewport::new(base, base.content, 1.0, [1000.0, 0.0]).is_none());
        assert_eq!(
            identity.vertices(),
            [
                [-1.0, -1.0, 0.0, 1.0],
                [1.0, -1.0, 1.0, 1.0],
                [-1.0, 1.0, 0.0, 0.0],
                [-1.0, 1.0, 0.0, 0.0],
                [1.0, -1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0, 0.0]
            ]
        );
        let clip = PixelRect {
            x: 10,
            y: 10,
            width: 80,
            height: 80,
        };
        let v = TransformedViewport::new(base, clip, 1.25, [-5.5, 4.25]).unwrap();
        assert_eq!(v.origin(), [-18.0, -8.25]);
        assert_eq!(v.extent(), [125.0, 125.0]);
        assert_eq!(v.map_pointer(10.0, 10.0), Some(PixelPoint { x: 22, y: 14 }));
        assert_eq!(v.map_pointer(9.99, 10.0), None);
        assert_eq!(v.map_pointer(90.0, 89.0), None);
        assert!(TransformedViewport::new(base, clip, f32::NAN, [0.0; 2]).is_none());
        assert!(TransformedViewport::new(base, clip, 1.0, [f32::MAX; 2]).is_none());
        assert!(TransformedViewport::new(
            base,
            PixelRect {
                x: u32::MAX,
                ..clip
            },
            1.0,
            [0.0; 2]
        )
        .is_none());
    }
}
