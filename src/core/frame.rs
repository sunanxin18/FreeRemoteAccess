use std::error::Error;
use std::fmt;

const MAX_SURFACE_PIXELS: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePixelFormat {
    Bgra8Srgb,
    Rgba8Srgb,
}

impl RemotePixelFormat {
    pub const fn bytes_per_pixel(self) -> u32 {
        4
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl FrameRect {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Result<Self, FrameContractError> {
        if width == 0 || height == 0 {
            return Err(FrameContractError::new("frame_rect_dimensions_invalid"));
        }
        x.checked_add(width)
            .ok_or_else(|| FrameContractError::new("frame_rect_overflow"))?;
        y.checked_add(height)
            .ok_or_else(|| FrameContractError::new("frame_rect_overflow"))?;
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    pub const fn x(self) -> u32 {
        self.x
    }

    pub const fn y(self) -> u32 {
        self.y
    }

    pub const fn width(self) -> u32 {
        self.width
    }

    pub const fn height(self) -> u32 {
        self.height
    }

    pub fn fits_within(self, width: u32, height: u32) -> bool {
        self.x
            .checked_add(self.width)
            .is_some_and(|right| right <= width)
            && self
                .y
                .checked_add(self.height)
                .is_some_and(|bottom| bottom <= height)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderUpdate {
    Reset {
        generation: u64,
        width: u32,
        height: u32,
        format: RemotePixelFormat,
    },
    DirtyRect {
        generation: u64,
        rect: FrameRect,
        format: RemotePixelFormat,
        bytes_per_row: u32,
        pixels: Box<[u8]>,
    },
    Present {
        generation: u64,
    },
}

impl RenderUpdate {
    pub fn reset(
        generation: u64,
        width: u32,
        height: u32,
        format: RemotePixelFormat,
    ) -> Result<Self, FrameContractError> {
        validate_surface_dimensions(width, height)?;
        Ok(Self::Reset {
            generation,
            width,
            height,
            format,
        })
    }

    pub fn dirty_rect(
        generation: u64,
        rect: FrameRect,
        format: RemotePixelFormat,
        bytes_per_row: u32,
        pixels: Box<[u8]>,
    ) -> Result<Self, FrameContractError> {
        let minimum_stride = rect
            .width
            .checked_mul(format.bytes_per_pixel())
            .ok_or_else(|| FrameContractError::new("dirty_rect_stride_overflow"))?;
        if bytes_per_row < minimum_stride {
            return Err(FrameContractError::new("dirty_rect_stride_too_small"));
        }
        let expected_length = u64::from(bytes_per_row)
            .checked_mul(u64::from(rect.height))
            .and_then(|length| usize::try_from(length).ok())
            .ok_or_else(|| FrameContractError::new("dirty_rect_length_overflow"))?;
        if pixels.len() != expected_length {
            return Err(FrameContractError::new("dirty_rect_length_mismatch"));
        }
        Ok(Self::DirtyRect {
            generation,
            rect,
            format,
            bytes_per_row,
            pixels,
        })
    }

    pub const fn present(generation: u64) -> Self {
        Self::Present { generation }
    }

    pub const fn generation(&self) -> u64 {
        match self {
            Self::Reset { generation, .. }
            | Self::DirtyRect { generation, .. }
            | Self::Present { generation } => *generation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationDisposition {
    Stale,
    Current,
    Future,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteSurfaceState {
    generation: u64,
    width: u32,
    height: u32,
}

impl RemoteSurfaceState {
    pub fn new(generation: u64, width: u32, height: u32) -> Result<Self, FrameContractError> {
        validate_surface_dimensions(width, height)?;
        Ok(Self {
            generation,
            width,
            height,
        })
    }

    pub const fn generation(self) -> u64 {
        self.generation
    }

    pub const fn dimensions(self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub const fn classify_generation(self, generation: u64) -> GenerationDisposition {
        if generation < self.generation {
            GenerationDisposition::Stale
        } else if generation == self.generation {
            GenerationDisposition::Current
        } else {
            GenerationDisposition::Future
        }
    }

    pub fn contains(self, rect: FrameRect) -> bool {
        rect.fits_within(self.width, self.height)
    }
}

fn validate_surface_dimensions(width: u32, height: u32) -> Result<(), FrameContractError> {
    if width == 0 || height == 0 {
        return Err(FrameContractError::new("surface_dimensions_invalid"));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| FrameContractError::new("surface_dimensions_overflow"))?;
    if pixels > MAX_SURFACE_PIXELS {
        return Err(FrameContractError::new("surface_dimensions_too_large"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameContractError {
    code: &'static str,
}

impl FrameContractError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for FrameContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "远程画面契约无效 ({})", self.code)
    }
}

impl Error for FrameContractError {}
