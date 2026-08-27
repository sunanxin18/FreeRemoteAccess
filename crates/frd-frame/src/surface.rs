use frd_core::{PixelRect, PixelSize, SessionId};

/// 渲染器可直接消费的四字节像素布局。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    Bgrx8UnormSrgb,
    Bgra8UnormSrgb,
    Rgba8UnormSrgb,
}

impl PixelFormat {
    pub(crate) const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Bgrx8UnormSrgb | Self::Bgra8UnormSrgb | Self::Rgba8UnormSrgb => 4,
        }
    }
}

/// 唯一拥有像素数据的缓冲区，避免在生产者和渲染器之间复制整帧。
///
/// ```compile_fail
/// use frd_frame::PixelBuffer;
///
/// let pixels = PixelBuffer::new(vec![0; 4]);
/// let copied = pixels.clone();
/// ```
#[derive(Debug)]
pub struct PixelBuffer(Box<[u8]>);

impl PixelBuffer {
    pub fn new(pixels: Vec<u8>) -> Self {
        Self(pixels.into_boxed_slice())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// 单个矩形区域的像素数据；行间的填充由 `stride_bytes` 表示。
#[derive(Debug)]
pub struct PixelPatch {
    pub rect: PixelRect,
    pub stride_bytes: u32,
    pub pixels: PixelBuffer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameCompleteness {
    Incremental,
    FullBaseline,
}

/// 从解码端交给渲染端的世代绑定更新。
#[derive(Debug)]
pub enum SurfaceUpdate {
    Reset {
        session_id: SessionId,
        generation: u64,
        size: PixelSize,
        format: PixelFormat,
    },
    Damage {
        session_id: SessionId,
        generation: u64,
        revision: u64,
        patches: Vec<PixelPatch>,
    },
    FrameBoundary {
        session_id: SessionId,
        generation: u64,
        revision: u64,
        completeness: FrameCompleteness,
    },
}
