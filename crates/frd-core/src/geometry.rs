#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelSize {
    pub width: u32,
    pub height: u32,
}

impl PixelSize {
    pub fn new(width: u32, height: u32) -> Option<Self> {
        (width != 0 && height != 0).then_some(Self { width, height })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelPoint {
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub fn checked_bounds(self) -> Option<(PixelPoint, PixelPoint)> {
        if self.width == 0 || self.height == 0 {
            return None;
        }

        Some((
            PixelPoint {
                x: self.x,
                y: self.y,
            },
            PixelPoint {
                x: self.x.checked_add(self.width)?,
                y: self.y.checked_add(self.height)?,
            },
        ))
    }
}

pub struct PhysicalViewport {
    pub drawable: PixelSize,
    pub content: PixelRect,
    pub remote: PixelSize,
}

impl PhysicalViewport {
    pub fn new(drawable: PixelSize, content: PixelRect, remote: PixelSize) -> Option<Self> {
        if drawable.width == 0 || drawable.height == 0 || remote.width == 0 || remote.height == 0 {
            return None;
        }

        let (_, content_end) = content.checked_bounds()?;
        (content_end.x <= drawable.width && content_end.y <= drawable.height).then_some(Self {
            drawable,
            content,
            remote,
        })
    }
}
