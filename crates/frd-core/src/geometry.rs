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
