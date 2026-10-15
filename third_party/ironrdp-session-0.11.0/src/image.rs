use std::sync::Arc;

use ironrdp_core::assert_impl;
use ironrdp_graphics::color_conversion::{rdp_15bit_to_rgb, rdp_16bit_to_rgb};
use ironrdp_graphics::image_processing::{ImageRegion, ImageRegionMut, PixelFormat};
use ironrdp_graphics::legacy_native::{KernelError, NativeKernels, PackedDepth, Pixel32Format, PixelRowSource};
use ironrdp_graphics::pointer::DecodedPointer;
use ironrdp_graphics::rectangle_processing::Region;
use ironrdp_graphics::rle::MAX_NATIVE_BITMAP_BYTES;
use ironrdp_pdu::geometry::{InclusiveRectangle, Rectangle as _};
use tracing::{debug, trace};

use crate::{SessionResult, custom_err};

const TILE_SIZE: u16 = 64;

/// 一次调用的原输出与实际成功内容；错误不返回部分 receipts。
#[derive(Debug)]
pub struct ActiveStageProcessResult {
    pub outputs: Vec<crate::ActiveStageOutput>,
    pub decoded_coverage: Vec<DecodedCoverageReceipt>,
}

/// 对应 outputs 中的 GraphicsUpdate；空数组明确表示只有展示 damage。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedCoverageReceipt {
    pub graphics_output_index: usize,
    pub rectangles: Vec<InclusiveRectangle>,
}

/// 每次调用至多 65536 个矩形与 65536 个 receipt，二者独立计费。
pub(crate) const MAX_COVERAGE_ENTRIES: usize = 65_536;
#[derive(Debug)]
pub(crate) enum CoverageError {
    Budget,
    Allocation(std::collections::TryReserveError),
}
impl core::fmt::Display for CoverageError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Budget => f.write_str("实际内容覆盖元数据超出单次调用限额"),
            Self::Allocation(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for CoverageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocation(e) => Some(e),
            Self::Budget => None,
        }
    }
}

/// 禁用时所有函数立即返回，Vec::new 不分配；不会执行 rich 几何与资源门。
#[derive(Default)]
pub(crate) struct CoverageCollector {
    pub(crate) enabled: bool,
    pub(crate) receipts: Vec<DecodedCoverageReceipt>,
    pending: Vec<InclusiveRectangle>,
    total_rectangles: usize,
    update_offset: usize,
}
impl CoverageCollector {
    pub(crate) fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
    pub(crate) fn reserve_entry(&mut self) -> SessionResult<()> {
        self.reserve_entries(1)
    }
    pub(crate) fn reserve_entries(&mut self, count: usize) -> SessionResult<()> {
        if !self.enabled {
            return Ok(());
        }
        if self
            .receipts
            .len()
            .checked_add(count)
            .is_none_or(|n| n > MAX_COVERAGE_ENTRIES)
        {
            return Err(custom_err!("decoded coverage", CoverageError::Budget));
        }
        self.receipts
            .try_reserve_exact(count)
            .map_err(|e| custom_err!("decoded coverage", CoverageError::Allocation(e)))
    }
    pub(crate) fn reserve_rectangles(&mut self, count: usize) -> SessionResult<()> {
        if !self.enabled {
            return Ok(());
        }
        if self
            .total_rectangles
            .checked_add(count)
            .is_none_or(|n| n > MAX_COVERAGE_ENTRIES)
        {
            return Err(custom_err!("decoded coverage", CoverageError::Budget));
        }
        self.pending
            .try_reserve_exact(count)
            .map_err(|e| custom_err!("decoded coverage", CoverageError::Allocation(e)))
    }
    pub(crate) fn content(&mut self, rectangles: impl IntoIterator<Item = InclusiveRectangle>) {
        if !self.enabled {
            return;
        }
        for rect in rectangles {
            self.pending.push(rect);
            self.total_rectangles += 1;
        }
    }
    pub(crate) fn finish_updates(&mut self, updates: &[crate::fast_path::UpdateKind]) -> SessionResult<()> {
        if !self.enabled {
            return Ok(());
        }
        for (index, update) in updates.iter().enumerate() {
            if matches!(update, crate::fast_path::UpdateKind::Region(_)) {
                self.reserve_entry()?;
                self.receipts.push(DecodedCoverageReceipt {
                    graphics_output_index: self.update_offset + index,
                    rectangles: core::mem::take(&mut self.pending),
                });
            }
        }
        debug_assert!(self.pending.is_empty(), "内容写入必须对应 GraphicsUpdate");
        self.update_offset += updates.len();
        Ok(())
    }
}

pub struct DecodedImage {
    pixel_format: PixelFormat,
    data: Vec<u8>,

    /// Part of the pointer image which should be drawn
    pointer_src_rect: InclusiveRectangle,
    /// X position of the pointer sprite on the screen
    pointer_draw_x: u16,
    /// Y position of the pointer sprite on the screen
    pointer_draw_y: u16,

    pointer_x: u16,
    pointer_y: u16,

    pointer: Option<Arc<DecodedPointer>>,
    /// Image data, overridden by pointer. Used to restore image after pointer was hidden or moved
    pointer_backbuffer: Vec<u8>,
    /// Whether to show pointer or not
    show_pointer: bool,
    /// Whether pointer is visible on the screen or its sprite is currently out of bounds
    pointer_visible_on_screen: bool,

    width: u16,
    height: u16,
}

assert_impl!(DecodedImage: Send);

impl core::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("pixel_format", &self.pixel_format)
            .field("data_len", &self.data.len())
            .field("pointer_src_rect", &self.pointer_src_rect)
            .field("pointer_draw_x", &self.pointer_draw_x)
            .field("pointer_draw_y", &self.pointer_draw_y)
            .field("pointer_x", &self.pointer_x)
            .field("pointer_y", &self.pointer_y)
            .field("pointer", &self.pointer)
            .field("pointer_backbuffer", &self.pointer_backbuffer)
            .field("show_pointer", &self.show_pointer)
            .field("pointer_visible_on_screen", &self.pointer_visible_on_screen)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

#[derive(PartialEq, Eq)]
enum PointerLayer {
    Background,
    Pointer,
}

struct PointerRenderingState {
    kernels: Option<NativeKernels>,
    redraw: bool,
    update_rectangle: InclusiveRectangle,
}

// 仅几何小值；预检不克隆 framebuffer，也不建立第二套 cursor 状态机。
#[derive(Clone)]
struct PointerGeometry {
    src: InclusiveRectangle,
    draw_x: u16,
    draw_y: u16,
    visible: bool,
}

/// 调用者已对整个操作预检；叶子不解析坐标，且所有合法 byte 的合成都是 total。
#[expect(clippy::too_many_arguments)]
fn copy_cursor_data_with_kernels(
    from: &[u8],
    from_pos: (usize, usize),
    from_stride: usize,
    to: &mut [u8],
    to_stride: usize,
    to_pos: (usize, usize),
    size: (usize, usize),
    dst_size: (usize, usize),
    composite: bool,
    kernels: Option<&NativeKernels>,
) -> SessionResult<()> {
    let Some(kernels) = kernels else {
        copy_cursor_data(
            from,
            from_pos,
            from_stride,
            to,
            to_stride,
            to_pos,
            size,
            dst_size,
            composite,
        );
        return Ok(());
    };
    if to_pos.0.checked_add(size.0).is_none_or(|x| x > dst_size.0)
        || to_pos.1.checked_add(size.1).is_none_or(|y| y > dst_size.1)
    {
        return Ok(());
    }
    let bytes = size
        .0
        .checked_mul(4)
        .ok_or_else(|| custom_err!("native cursor geometry", KernelError::InvalidDimensions))?;
    // 所有行在首行写入前预检，不能在第二行错误后留下半张 cursor。
    cursor_span(from.len(), from_pos, from_stride, size)?;
    cursor_span(to.len(), to_pos, to_stride, size)?;
    for row in 0..size.1 {
        let a = (from_pos.1 + row) * from_stride + from_pos.0 * 4;
        let b = (to_pos.1 + row) * to_stride + to_pos.0 * 4;
        if composite {
            // background/output 借用不重叠。固定四像素小 staging 包含短尾，
            // 像素运算仍全部进入同一 checked SIMD row/core，不 clone 整行或整帧。
            for (source, destination) in from[a..a + bytes].chunks(16).zip(to[b..b + bytes].chunks_mut(16)) {
                let mut background = [0u8; 16];
                kernels
                    .copy_u8(destination, &mut background[..destination.len()])
                    .map_err(|e| custom_err!("native cursor background", e))?;
                kernels
                    .composite_cursor_row(source, &background[..destination.len()], destination)
                    .map_err(|e| custom_err!("native cursor composite", e))?;
            }
            #[cfg(test)]
            NATIVE_CURSOR_CALLS.with(|v| v.borrow_mut().push(("cursor_composite", bytes)));
        } else {
            kernels
                .copy_u8(&from[a..a + bytes], &mut to[b..b + bytes])
                .map_err(|e| custom_err!("native cursor copy", e))?;
            #[cfg(test)]
            NATIVE_CURSOR_CALLS.with(|v| v.borrow_mut().push(("cursor_copy", bytes)));
        }
    }
    Ok(())
}
fn cursor_span(length: usize, pos: (usize, usize), stride: usize, size: (usize, usize)) -> SessionResult<()> {
    let end = size
        .1
        .checked_sub(1)
        .and_then(|last| pos.1.checked_add(last))
        .and_then(|y| y.checked_mul(stride))
        .and_then(|n| pos.0.checked_mul(4).and_then(|x| n.checked_add(x)))
        .and_then(|n| size.0.checked_mul(4).and_then(|w| n.checked_add(w)))
        .ok_or_else(|| custom_err!("native cursor geometry", KernelError::InvalidDimensions))?;
    if end > length {
        return Err(custom_err!("native cursor span", KernelError::Length));
    }
    Ok(())
}

#[expect(clippy::too_many_arguments)]
fn copy_cursor_data(
    from: &[u8],
    from_pos: (usize, usize),
    from_stride: usize,
    to: &mut [u8],
    to_stride: usize,
    to_pos: (usize, usize),
    size: (usize, usize),
    dst_size: (usize, usize),
    composite: bool,
) {
    const PIXEL_SIZE: usize = 4;

    if to_pos.0 + size.0 > dst_size.0 || to_pos.1 + size.1 > dst_size.1 {
        // Perform clipping
        return;
    }

    let (from_x, from_y) = from_pos;
    let (to_x, to_y) = to_pos;
    let (width, height) = size;

    for y in 0..height {
        let from_start = (from_y + y) * from_stride + from_x * PIXEL_SIZE;
        let to_start = (to_y + y) * to_stride + to_x * PIXEL_SIZE;

        if composite {
            for pixel in 0..width {
                let dest_r = to[to_start + pixel * PIXEL_SIZE];
                let dest_g = to[to_start + pixel * PIXEL_SIZE + 1];
                let dest_b = to[to_start + pixel * PIXEL_SIZE + 2];

                let src_r = from[from_start + pixel * PIXEL_SIZE];
                let src_g = from[from_start + pixel * PIXEL_SIZE + 1];
                let src_b = from[from_start + pixel * PIXEL_SIZE + 2];
                let src_a = from[from_start + pixel * PIXEL_SIZE + 3];

                // Inverted pixel, this color has a special meaning when encoded by ironrdp-graphics
                if src_a == 0 && src_r == 255 && src_g == 255 && src_b == 255 {
                    to[to_start + pixel * PIXEL_SIZE] = 255 - dest_r;
                    to[to_start + pixel * PIXEL_SIZE + 1] = 255 - dest_g;
                    to[to_start + pixel * PIXEL_SIZE + 2] = 255 - dest_b;
                    to[to_start + pixel * PIXEL_SIZE + 3] = 255;
                    continue;
                }

                // Skip 100% transparent pixels
                if src_a == 0 {
                    continue;
                }

                #[expect(clippy::as_conversions, reason = "(u16 >> 8) fits into u8 + hot loop")]
                {
                    // Integer alpha blending, source represented as premultiplied alpha color, calculation in floating point
                    to[to_start + pixel * PIXEL_SIZE] =
                        src_r + ((u16::from(dest_r) * u16::from(255 - src_a)) >> 8) as u8;
                    to[to_start + pixel * PIXEL_SIZE + 1] =
                        src_g + ((u16::from(dest_g) * u16::from(255 - src_a)) >> 8) as u8;
                    to[to_start + pixel * PIXEL_SIZE + 2] =
                        src_b + ((u16::from(dest_b) * u16::from(255 - src_a)) >> 8) as u8;
                    // Framebuffer is always opaque, so we can skip alpha channel change
                }
            }
        } else {
            to[to_start..to_start + width * PIXEL_SIZE]
                .copy_from_slice(&from[from_start..from_start + width * PIXEL_SIZE]);
        }
    }
}

// 封闭八格式映射；协议适配器不能传任意 channel offsets。
pub(crate) fn native_pixel_format(format: PixelFormat) -> Pixel32Format {
    match format {
        PixelFormat::ARgb32 => Pixel32Format::ARgb,
        PixelFormat::XRgb32 => Pixel32Format::XRgb,
        PixelFormat::ABgr32 => Pixel32Format::ABgr,
        PixelFormat::XBgr32 => Pixel32Format::XBgr,
        PixelFormat::BgrA32 => Pixel32Format::BgrA,
        PixelFormat::BgrX32 => Pixel32Format::BgrX,
        PixelFormat::RgbA32 => Pixel32Format::RgbA,
        PixelFormat::RgbX32 => Pixel32Format::RgbX,
    }
}
fn native_source_bytes(format: PixelRowSource) -> usize {
    match format {
        PixelRowSource::Packed(PackedDepth::Index8) => 1,
        PixelRowSource::Packed(PackedDepth::Rgb555 | PackedDepth::Rgb565) => 2,
        PixelRowSource::Packed(PackedDepth::Bgr24) | PixelRowSource::Rgb24 => 3,
        PixelRowSource::Raw32(_) | PixelRowSource::Region32(_) => 4,
    }
}
fn native_rect_size(rect: &InclusiveRectangle) -> Result<(usize, usize), KernelError> {
    if rect.left > rect.right || rect.top > rect.bottom {
        return Err(KernelError::InvalidDimensions);
    }
    Ok((
        usize::from(rect.right) - usize::from(rect.left) + 1,
        usize::from(rect.bottom) - usize::from(rect.top) + 1,
    ))
}
#[cfg(test)]
std::thread_local! {
    static NATIVE_CURSOR_CALLS: std::cell::RefCell<Vec<(&'static str, usize)>> = const { std::cell::RefCell::new(Vec::new()) };
    static NATIVE_IMAGE_CALLS: std::cell::RefCell<Vec<(&'static str, usize)>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl DecodedImage {
    pub fn new(pixel_format: PixelFormat, width: u16, height: u16) -> Self {
        let len = usize::from(width) * usize::from(height) * usize::from(pixel_format.bytes_per_pixel());

        Self {
            pixel_format,
            data: vec![0; len],
            width,
            height,

            pointer_src_rect: InclusiveRectangle {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            pointer_x: 0,
            pointer_y: 0,
            pointer_draw_x: 0,
            pointer_draw_y: 0,
            pointer_backbuffer: Vec::new(),
            pointer: None,
            show_pointer: false,
            pointer_visible_on_screen: true,
        }
    }

    pub fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn bytes_per_pixel(&self) -> usize {
        usize::from(self.pixel_format.bytes_per_pixel())
    }

    pub fn stride(&self) -> usize {
        usize::from(self.width) * self.bytes_per_pixel()
    }

    pub fn data_for_rect(&self, rect: &InclusiveRectangle) -> &[u8] {
        let start = usize::from(rect.left) * self.bytes_per_pixel() + usize::from(rect.top) * self.stride();
        let end =
            start + usize::from(rect.height() - 1) * self.stride() + usize::from(rect.width()) * self.bytes_per_pixel();
        &self.data[start..end]
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// Returns `true` if the rectangle fits entirely within the image bounds.
    fn rect_fits(&self, rect: &InclusiveRectangle) -> bool {
        rect.right < self.width && rect.bottom < self.height
    }

    fn apply_pointer_layer_with_kernels(
        &mut self,
        layer: PointerLayer,
        kernels: Option<&NativeKernels>,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        // Pointer is not hidden, but its texture is not visible on the screen, so we don't
        // need to render it
        if layer == PointerLayer::Pointer && !self.pointer_visible_on_screen {
            return Ok(None);
        }

        if self.data.is_empty() {
            return Ok(None);
        }

        let pointer = if let Some(pointer) = &self.pointer {
            pointer
        } else {
            return Ok(None);
        };

        if self.pointer_src_rect.width() == 0 || self.pointer_src_rect.height() == 0 {
            return Ok(None);
        }

        let dest_rect = InclusiveRectangle {
            left: self.pointer_draw_x,
            top: self.pointer_draw_y,
            right: self.pointer_draw_x + self.pointer_src_rect.width() - 1,
            bottom: self.pointer_draw_y + self.pointer_src_rect.height() - 1,
        };

        if dest_rect.width() == 0 || dest_rect.height() == 0 {
            return Ok(None);
        }

        let pointer_src_rect_width = usize::from(self.pointer_src_rect.width());
        let pointer_src_rect_height = usize::from(self.pointer_src_rect.height());
        let pointer_draw_x = usize::from(self.pointer_draw_x);
        let pointer_draw_y = usize::from(self.pointer_draw_y);
        let width = usize::from(self.width);
        let height = usize::from(self.height);

        match &layer {
            PointerLayer::Background => {
                if self.pointer_backbuffer.is_empty() {
                    // Backbuffer were previously empty
                    return Ok(None);
                }

                copy_cursor_data_with_kernels(
                    &self.pointer_backbuffer,
                    (0, 0),
                    pointer_src_rect_width * 4,
                    &mut self.data,
                    width * 4,
                    (pointer_draw_x, pointer_draw_y),
                    (pointer_src_rect_width, pointer_src_rect_height),
                    (width, height),
                    false,
                    kernels,
                )?;
            }
            PointerLayer::Pointer => {
                // Copy current background to backbuffer
                let buffer_size = self
                    .pointer_backbuffer
                    .len()
                    .max(pointer_src_rect_width * pointer_src_rect_height * 4);
                self.pointer_backbuffer.resize(buffer_size, 0);

                copy_cursor_data_with_kernels(
                    &self.data,
                    (pointer_draw_x, pointer_draw_y),
                    width * 4,
                    &mut self.pointer_backbuffer,
                    pointer_src_rect_width * 4,
                    (0, 0),
                    (pointer_src_rect_width, pointer_src_rect_height),
                    (width, height),
                    false,
                    kernels,
                )?;

                // Draw pointer (with compositing)
                copy_cursor_data_with_kernels(
                    pointer.bitmap_data.as_slice(),
                    (
                        usize::from(self.pointer_src_rect.left),
                        usize::from(self.pointer_src_rect.top),
                    ),
                    usize::from(pointer.width) * 4,
                    &mut self.data,
                    width * 4,
                    (pointer_draw_x, pointer_draw_y),
                    (pointer_src_rect_width, pointer_src_rect_height),
                    (width, height),
                    true,
                    kernels,
                )?;
            }
        }

        // Request redraw of the changed area
        Ok(Some(dest_rect))
    }

    #[allow(dead_code)] // Compatibility 入口保持原语义。
    pub(crate) fn show_pointer(&mut self) -> SessionResult<Option<InclusiveRectangle>> {
        self.show_pointer_with_kernels(None)
    }

    pub(crate) fn show_pointer_with_kernels(
        &mut self,
        kernels: Option<&NativeKernels>,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        if !self.show_pointer {
            if kernels.is_some() {
                self.preflight_pointer_layers(None, None)?;
            }
            self.show_pointer = true;
            self.apply_pointer_layer_with_kernels(PointerLayer::Pointer, kernels)
        } else {
            Ok(None)
        }
    }

    #[allow(dead_code)] // Compatibility 入口保持原语义。
    pub(crate) fn hide_pointer(&mut self) -> SessionResult<Option<InclusiveRectangle>> {
        self.hide_pointer_with_kernels(None)
    }

    pub(crate) fn hide_pointer_with_kernels(
        &mut self,
        kernels: Option<&NativeKernels>,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        if self.show_pointer {
            if kernels.is_some() {
                self.preflight_pointer_layers(None, None)?;
            }
            self.show_pointer = false;
            self.apply_pointer_layer_with_kernels(PointerLayer::Background, kernels)
        } else {
            Ok(None)
        }
    }

    /// 检查整个旧 restore 和未来 save/composite 的行范围，再预留所需 backbuffer。
    /// 合法合成采用 wrapping total 算术，因此无需像素预演/overflow scratch。
    fn preflight_pointer_layers(
        &mut self,
        future: Option<PointerGeometry>,
        new_pointer: Option<&DecodedPointer>,
    ) -> SessionResult<()> {
        if self.data.is_empty() {
            return Ok(());
        }
        let old = self.pointer_geometry();
        if let Some(pointer) = self.pointer.as_deref() {
            self.check_pointer_shape(pointer)?;
            if !self.pointer_backbuffer.is_empty() {
                let bytes = self.check_pointer_destination(&old)?;
                if self.pointer_backbuffer.len() < bytes {
                    return Err(custom_err!("native cursor background", KernelError::Length));
                }
            }
        }
        let pointer = new_pointer.or(self.pointer.as_deref());
        let geometry = future.as_ref().unwrap_or(&old);
        let required = if let Some(pointer) = pointer {
            self.check_pointer_shape(pointer)?;
            if geometry.visible {
                self.check_pointer_spans(pointer, geometry)?
            } else {
                0
            }
        } else {
            0
        };
        self.pointer_backbuffer
            .try_reserve_exact(required.saturating_sub(self.pointer_backbuffer.len()))
            .map_err(|e| custom_err!("native cursor allocation", e))?;
        Ok(())
    }
    fn check_pointer_shape(&self, pointer: &DecodedPointer) -> SessionResult<()> {
        let bytes = usize::from(pointer.width)
            .checked_mul(usize::from(pointer.height))
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| custom_err!("native cursor geometry", KernelError::InvalidDimensions))?;
        if bytes == 0 || bytes > MAX_NATIVE_BITMAP_BYTES || bytes > isize::MAX as usize {
            return Err(custom_err!(
                "native cursor resource limit",
                KernelError::InvalidDimensions
            ));
        }
        if bytes != pointer.bitmap_data.len() {
            return Err(custom_err!("native cursor bitmap", KernelError::Length));
        }
        Ok(())
    }
    fn check_pointer_spans(&self, pointer: &DecodedPointer, geometry: &PointerGeometry) -> SessionResult<usize> {
        let bytes = self.check_pointer_destination(geometry)?;
        let size = native_rect_size(&geometry.src).map_err(|e| custom_err!("native cursor geometry", e))?;
        let src = (usize::from(geometry.src.left), usize::from(geometry.src.top));
        if src.0.checked_add(size.0).is_none_or(|v| v > usize::from(pointer.width))
            || src
                .1
                .checked_add(size.1)
                .is_none_or(|v| v > usize::from(pointer.height))
        {
            return Err(custom_err!("native cursor geometry", KernelError::InvalidDimensions));
        }
        cursor_span(pointer.bitmap_data.len(), src, usize::from(pointer.width) * 4, size)?;
        Ok(bytes)
    }
    fn check_pointer_destination(&self, geometry: &PointerGeometry) -> SessionResult<usize> {
        let size = native_rect_size(&geometry.src).map_err(|e| custom_err!("native cursor geometry", e))?;
        let dst = (usize::from(geometry.draw_x), usize::from(geometry.draw_y));
        if dst.0.checked_add(size.0).is_none_or(|v| v > usize::from(self.width))
            || dst.1.checked_add(size.1).is_none_or(|v| v > usize::from(self.height))
        {
            return Err(custom_err!("native cursor geometry", KernelError::InvalidDimensions));
        }
        cursor_span(self.data.len(), dst, usize::from(self.width) * 4, size)?;
        size.0
            .checked_mul(size.1)
            .and_then(|n| n.checked_mul(4))
            .filter(|n| *n <= MAX_NATIVE_BITMAP_BYTES && *n <= isize::MAX as usize)
            .ok_or_else(|| custom_err!("native cursor resource limit", KernelError::InvalidDimensions))
    }

    fn pointer_geometry(&self) -> PointerGeometry {
        PointerGeometry {
            src: self.pointer_src_rect.clone(),
            draw_x: self.pointer_draw_x,
            draw_y: self.pointer_draw_y,
            visible: self.pointer_visible_on_screen,
        }
    }
    fn set_pointer_geometry(&mut self, geometry: PointerGeometry) {
        self.pointer_src_rect = geometry.src;
        self.pointer_draw_x = geometry.draw_x;
        self.pointer_draw_y = geometry.draw_y;
        self.pointer_visible_on_screen = geometry.visible;
    }
    fn calculate_pointer_geometry(
        &self,
        pointer: Option<&DecodedPointer>,
        x: u16,
        y: u16,
        show: bool,
        checked: bool,
    ) -> SessionResult<PointerGeometry> {
        let mut geometry = self.pointer_geometry();
        let pointer = match pointer {
            Some(pointer) if show => pointer,
            _ => return Ok(geometry),
        };
        if checked
            && (self.width == 0
                || self.height == 0
                || pointer.width == 0
                || pointer.height == 0
                || (x >= pointer.hotspot_x && x - pointer.hotspot_x == u16::MAX)
                || (y >= pointer.hotspot_y && y - pointer.hotspot_y == u16::MAX))
        {
            return Err(custom_err!("native cursor geometry", KernelError::InvalidDimensions));
        }
        let left_virtual = i32::from(x) - i32::from(pointer.hotspot_x);
        let top_virtual = i32::from(y) - i32::from(pointer.hotspot_y);
        let right_virtual = left_virtual + i32::from(pointer.width) - 1;
        let bottom_virtual = top_virtual + i32::from(pointer.height) - 1;

        let (left, draw_x) = if left_virtual < 0 {
            // Cut left side if required
            (pointer.hotspot_x - x, 0)
        } else {
            (0, x - pointer.hotspot_x)
        };

        let (top, draw_y) = if top_virtual < 0 {
            // Cut top side if required
            (pointer.hotspot_y - y, 0)
        } else {
            (0, y - pointer.hotspot_y)
        };

        // Cut right side if required
        let right = if right_virtual >= i32::from(self.width - 1) {
            if draw_x + 1 >= self.width {
                // Pointer is completely out of bounds horizontally
                geometry.visible = false;
                return Ok(geometry);
            } else {
                self.width - (draw_x + 1)
            }
        } else {
            pointer.width - 1
        };

        // Cut bottom side if required
        let bottom = if bottom_virtual >= i32::from(self.height - 1) {
            if (draw_y + 1) >= self.height {
                // Pointer is completely out of bounds vertically
                geometry.visible = false;
                return Ok(geometry);
            } else {
                self.height - (draw_y + 1)
            }
        } else {
            pointer.height - 1
        };

        geometry.visible = true;

        let pointer_src_rect = InclusiveRectangle {
            left,
            top,
            right,
            bottom,
        };

        geometry.src = pointer_src_rect;
        geometry.draw_x = draw_x;
        geometry.draw_y = draw_y;
        if checked {
            native_rect_size(&geometry.src).map_err(|e| custom_err!("native cursor geometry", e))?;
        }
        Ok(geometry)
    }

    #[allow(dead_code)] // Compatibility 入口保持原语义。
    pub(crate) fn move_pointer(&mut self, x: u16, y: u16) -> SessionResult<Option<InclusiveRectangle>> {
        self.move_pointer_with_kernels(x, y, None)
    }

    pub(crate) fn move_pointer_with_kernels(
        &mut self,
        x: u16,
        y: u16,
        kernels: Option<&NativeKernels>,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        if kernels.is_some() {
            let geometry = self.calculate_pointer_geometry(self.pointer.as_deref(), x, y, self.show_pointer, true)?;
            self.preflight_pointer_layers(Some(geometry), None)?;
        }
        self.pointer_x = x;
        self.pointer_y = y;

        if self.pointer.is_some() && self.show_pointer {
            let old_rect = self.apply_pointer_layer_with_kernels(PointerLayer::Background, kernels)?;
            let geometry = self.calculate_pointer_geometry(
                self.pointer.as_deref(),
                self.pointer_x,
                self.pointer_y,
                self.show_pointer,
                kernels.is_some(),
            )?;
            self.set_pointer_geometry(geometry);
            let new_rect = self.apply_pointer_layer_with_kernels(PointerLayer::Pointer, kernels)?;

            match (old_rect, new_rect) {
                (None, None) => Ok(None),
                (None, Some(rect)) => Ok(Some(rect)),
                (Some(rect), None) => Ok(Some(rect)),
                (Some(a), Some(b)) => Ok(Some(a.union(&b))),
            }
        } else {
            Ok(None)
        }
    }

    #[allow(dead_code)] // Compatibility 入口保持原语义。
    pub(crate) fn update_pointer(&mut self, pointer: Arc<DecodedPointer>) -> SessionResult<Option<InclusiveRectangle>> {
        self.update_pointer_with_kernels(pointer, None)
    }

    pub(crate) fn update_pointer_with_kernels(
        &mut self,
        pointer: Arc<DecodedPointer>,
        kernels: Option<&NativeKernels>,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        if kernels.is_some() {
            let geometry =
                self.calculate_pointer_geometry(Some(&pointer), self.pointer_x, self.pointer_y, true, true)?;
            self.preflight_pointer_layers(Some(geometry), Some(&pointer))?;
        }
        self.show_pointer = true;

        // Remove old pointer from frame buffer
        let old_rect = if self.pointer.is_some() {
            self.apply_pointer_layer_with_kernels(PointerLayer::Background, kernels)?
        } else {
            None
        };

        self.pointer = Some(pointer);
        let geometry = self.calculate_pointer_geometry(
            self.pointer.as_deref(),
            self.pointer_x,
            self.pointer_y,
            self.show_pointer,
            kernels.is_some(),
        )?;
        self.set_pointer_geometry(geometry);

        // Draw new pointer
        let new_rect = self.apply_pointer_layer_with_kernels(PointerLayer::Pointer, kernels)?;

        match (old_rect, new_rect) {
            (None, None) => Ok(None),
            (None, Some(rect)) => Ok(Some(rect)),
            (Some(rect), None) => Ok(Some(rect)),
            (Some(a), Some(b)) => Ok(Some(a.union(&b))),
        }
    }

    fn is_pointer_redraw_required(&self, update_rectangle: &InclusiveRectangle) -> bool {
        let pointer_dest_rect = InclusiveRectangle {
            left: self.pointer_draw_x,
            top: self.pointer_draw_y,
            right: self.pointer_draw_x + self.pointer_src_rect.width() - 1,
            bottom: self.pointer_draw_y + self.pointer_src_rect.height() - 1,
        };

        update_rectangle.intersect(&pointer_dest_rect).is_some() && self.show_pointer
    }

    /// This method should be called BEFORE and framebuffer updates, with the update rectangle,
    /// to determine if the pointer needs to be redrawn (overlapping with the update rectangle).
    fn pointer_rendering_begin(
        &mut self,
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<PointerRenderingState> {
        self.pointer_rendering_begin_with_kernels(update_rectangle, None)
    }
    fn pointer_rendering_begin_with_kernels(
        &mut self,
        update_rectangle: &InclusiveRectangle,
        kernels: Option<&NativeKernels>,
    ) -> SessionResult<PointerRenderingState> {
        if kernels.is_some() {
            self.preflight_pointer_layers(None, None)?;
        }
        if !self.is_pointer_redraw_required(update_rectangle) || self.pointer.is_none() {
            return Ok(PointerRenderingState {
                kernels: kernels.copied(),
                redraw: false,
                update_rectangle: update_rectangle.clone(),
            });
        }

        let state = self
            .apply_pointer_layer_with_kernels(PointerLayer::Background, kernels)?
            .map(|cursor_erase_rect| PointerRenderingState {
                kernels: kernels.copied(),
                redraw: true,
                update_rectangle: cursor_erase_rect.union(update_rectangle),
            })
            .unwrap_or_else(|| PointerRenderingState {
                kernels: kernels.copied(),
                redraw: false,
                update_rectangle: update_rectangle.clone(),
            });

        Ok(state)
    }

    fn pointer_rendering_end(
        &mut self,
        pointer_rendering_state: PointerRenderingState,
    ) -> SessionResult<InclusiveRectangle> {
        let kernels = pointer_rendering_state.kernels.as_ref();
        if !pointer_rendering_state.redraw {
            return Ok(pointer_rendering_state.update_rectangle);
        }

        let update_rectangle = self
            .apply_pointer_layer_with_kernels(PointerLayer::Pointer, kernels)?
            .map(|pointer_draw_rectangle| pointer_draw_rectangle.union(&pointer_rendering_state.update_rectangle))
            .unwrap_or_else(|| pointer_rendering_state.update_rectangle);

        Ok(update_rectangle)
    }

    // To apply the buffer, we need to un-apply previously drawn cursor, and then apply it again
    // in other position.

    /// 先检查完整源形状、资源上限和目标几何；供 portable adapter 在分配前使用。
    pub(crate) fn native_bitmap_size(
        &self,
        format: PixelRowSource,
        source_width: usize,
        source_height: usize,
        row_stride: usize,
        rectangle: &InclusiveRectangle,
    ) -> SessionResult<usize> {
        let (width, height) = native_rect_size(rectangle).map_err(|e| custom_err!("native bitmap geometry", e))?;
        let pixel_bytes = native_source_bytes(format);
        let row_bytes = source_width
            .checked_mul(pixel_bytes)
            .ok_or_else(|| custom_err!("native bitmap geometry", KernelError::InvalidDimensions))?;
        if source_width == 0
            || source_height == 0
            || source_width < width
            || source_height < height
            || row_stride < row_bytes
        {
            return Err(custom_err!("native bitmap geometry", KernelError::InvalidDimensions));
        }
        row_stride
            .checked_mul(source_height)
            .filter(|&n| n <= isize::MAX as usize && n <= MAX_NATIVE_BITMAP_BYTES)
            .ok_or_else(|| custom_err!("native bitmap resource limit", KernelError::InvalidDimensions))
    }

    /// 原生 Bitmap 的精确行接线；SW/SH 保持完整源布局，逻辑左上裁剪目标大小。
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn apply_bitmap_native(
        &mut self,
        source: &[u8],
        source_format: PixelRowSource,
        source_width: usize,
        source_height: usize,
        row_stride: usize,
        bottom_up: bool,
        rectangle: &InclusiveRectangle,
        palette: Option<&[[u8; 3]; 256]>,
        kernels: &NativeKernels,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        let expected = self.native_bitmap_size(source_format, source_width, source_height, row_stride, rectangle)?;
        if source.len() != expected {
            return Err(custom_err!("native bitmap source", KernelError::Length));
        }
        let (width, height) = native_rect_size(rectangle).map_err(|e| custom_err!("native bitmap geometry", e))?;
        let source_row_bytes = width
            .checked_mul(native_source_bytes(source_format))
            .ok_or_else(|| custom_err!("native bitmap geometry", KernelError::InvalidDimensions))?;
        let destination_row_bytes = width
            .checked_mul(4)
            .ok_or_else(|| custom_err!("native bitmap geometry", KernelError::InvalidDimensions))?;
        let destination_format = native_pixel_format(self.pixel_format);
        // 已构造能力不会猜 ISA；空行检查格式/能力/palette，所有可预测错误先于 cursor begin。
        kernels
            .convert_pixel_row(source_format, destination_format, &[], palette, &mut [])
            .map_err(|e| custom_err!("native bitmap capability", e))?;
        if !self.rect_fits(rectangle) {
            return Ok(None);
        }
        let destination_start = usize::from(rectangle.top) * self.stride() + usize::from(rectangle.left) * 4;
        for y in 0..height {
            let source_y = if bottom_up { source_height - 1 - y } else { y };
            let source_start = source_y * row_stride;
            let destination_start = destination_start + y * self.stride();
            if source.get(source_start..source_start + source_row_bytes).is_none()
                || self
                    .data
                    .get(destination_start..destination_start + destination_row_bytes)
                    .is_none()
            {
                return Err(custom_err!("native bitmap row", KernelError::Length));
            }
        }
        #[cfg(test)]
        NATIVE_IMAGE_CALLS.with(|v| v.borrow_mut().push(("begin", 0)));
        let state = self.pointer_rendering_begin_with_kernels(rectangle, Some(kernels))?;
        let stride = self.stride();
        for y in 0..height {
            let source_y = if bottom_up { source_height - 1 - y } else { y };
            let start = source_y * row_stride;
            let destination_start = destination_start + y * stride;
            #[cfg(test)]
            NATIVE_IMAGE_CALLS.with(|v| v.borrow_mut().push(("row", destination_row_bytes)));
            kernels
                .convert_pixel_row(
                    source_format,
                    destination_format,
                    &source[start..start + source_row_bytes],
                    palette,
                    &mut self.data[destination_start..destination_start + destination_row_bytes],
                )
                .map_err(|e| custom_err!("native bitmap row", e))?;
        }
        #[cfg(test)]
        NATIVE_IMAGE_CALLS.with(|v| v.borrow_mut().push(("end", 0)));
        self.pointer_rendering_end(state).map(Some)
    }

    /// rich 默认路径仅调整行布局；像素转换仍复用原版 helper，保持独立参考语义。
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn apply_bitmap_compat(
        &mut self,
        source: &[u8],
        format: PixelRowSource,
        sw: usize,
        sh: usize,
        stride: usize,
        bottom_up: bool,
        rectangle: &InclusiveRectangle,
        palette: &[[u8; 3]; 256],
    ) -> SessionResult<Option<InclusiveRectangle>> {
        let expected = self.native_bitmap_size(format, sw, sh, stride, rectangle)?;
        if source.len() != expected {
            return Err(custom_err!("完整 Bitmap 源", KernelError::Length));
        }
        let (dw, dh) = native_rect_size(rectangle).map_err(|e| custom_err!("Bitmap 目标", e))?;
        let row = dw
            .checked_mul(native_source_bytes(format))
            .ok_or_else(|| custom_err!("Bitmap 行", KernelError::InvalidDimensions))?;
        let size = row
            .checked_mul(dh)
            .filter(|&n| n <= MAX_NATIVE_BITMAP_BYTES && n <= isize::MAX as usize)
            .ok_or_else(|| custom_err!("Bitmap staging", KernelError::InvalidDimensions))?;
        if !self.rect_fits(rectangle) {
            return Ok(None);
        }
        // 完整源的逻辑左上角，bottom-up 必须按 SH 定位；staging 自身也保持 bottom-up。
        let mut cropped = Vec::new();
        cropped
            .try_reserve_exact(size)
            .map_err(|e| custom_err!("Bitmap staging allocation", e))?;
        for y in (0..dh).rev() {
            let sy = if bottom_up { sh - 1 - y } else { y };
            let start = sy * stride;
            cropped.extend_from_slice(&source[start..start + row]);
        }
        let damage = match format {
            PixelRowSource::Packed(PackedDepth::Index8) => {
                self.apply_rgb8_with_palette(&cropped, rectangle, palette)?
            }
            PixelRowSource::Packed(PackedDepth::Rgb555) => self.apply_rgb15_bitmap(&cropped, rectangle)?,
            PixelRowSource::Packed(PackedDepth::Rgb565) => self.apply_rgb16_bitmap(&cropped, rectangle)?,
            PixelRowSource::Packed(PackedDepth::Bgr24) => self.apply_bgr24_bitmap(&cropped, rectangle)?,
            PixelRowSource::Rgb24 => self.apply_rgb24(&cropped, rectangle, true)?,
            PixelRowSource::Raw32(Pixel32Format::BgrX) => {
                self.apply_rgb32_bitmap(&cropped, PixelFormat::BgrX32, rectangle)?
            }
            #[cfg(feature = "qoi")]
            PixelRowSource::Raw32(Pixel32Format::RgbA) => self.apply_rgba32(&cropped, rectangle, true)?,
            _ => {
                return Err(custom_err!("Bitmap 参考格式", KernelError::InvalidDimensions));
            }
        };
        Ok(Some(damage))
    }

    /// 完整 tile 已解码后，保持 Region32 alpha、region 顺序与固定 64 像素源 stride。
    pub(crate) fn apply_tile_native(
        &mut self,
        source: &[u8],
        source_format: PixelFormat,
        clips: &Region,
        update: &InclusiveRectangle,
        kernels: &NativeKernels,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        self.apply_tile_rows(source, source_format, clips, update, Some(kernels), false)
    }
    /// rich 已验证的精确交集；不再经过上游 Region 的 lossy simplify。
    pub(crate) fn apply_tile_exact(
        &mut self,
        source: &[u8],
        source_format: PixelFormat,
        regions: &Region,
        update: &InclusiveRectangle,
        kernels: Option<&NativeKernels>,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        self.apply_tile_rows(source, source_format, regions, update, kernels, true)
    }
    fn apply_tile_rows(
        &mut self,
        source: &[u8],
        source_format: PixelFormat,
        clips: &Region,
        update: &InclusiveRectangle,
        kernels: Option<&NativeKernels>,
        exact: bool,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        let (width, height) = native_rect_size(update).map_err(|e| custom_err!("native tile geometry", e))?;
        if width > 64 || height > 64 || source.len() != 64 * 64 * 4 {
            return Err(custom_err!("native tile source", KernelError::Length));
        }
        if clips.rectangles.is_empty() {
            return Ok(None);
        }
        native_rect_size(&clips.extents).map_err(|e| custom_err!("native tile clips", e))?;
        for rect in &clips.rectangles {
            native_rect_size(rect).map_err(|e| custom_err!("native tile clips", e))?;
        }
        let format = PixelRowSource::Region32(native_pixel_format(source_format));
        let destination_format = native_pixel_format(self.pixel_format);
        if let Some(kernels) = kernels {
            kernels
                .convert_pixel_row(format, destination_format, &[], None, &mut [])
                .map_err(|e| custom_err!("native tile capability", e))?;
        }
        if !self.rect_fits(&clips.extents) {
            return Ok(None);
        }
        let intersected;
        let region = if exact {
            clips
        } else {
            intersected = clips.intersect_rectangle(update);
            &intersected
        };
        if region.rectangles.is_empty() {
            return Ok(None);
        }
        // 全部 source/destination row 先验证，不让第二 region 几何错误发生在 pointer begin 后。
        for rect in &region.rectangles {
            let (width, height) = native_rect_size(rect).map_err(|e| custom_err!("native tile clips", e))?;
            if rect.left < update.left
                || rect.top < update.top
                || rect.right > update.right
                || rect.bottom > update.bottom
            {
                return Err(custom_err!("精确 tile 交集", KernelError::InvalidDimensions));
            }
            let x = usize::from(rect.left - update.left);
            let y = usize::from(rect.top - update.top);
            if x + width > 64 || y + height > 64 {
                return Err(custom_err!("native tile source", KernelError::Length));
            }
            for row in 0..height {
                let start = (y + row) * 64 * 4 + x * 4;
                let to = (usize::from(rect.top) + row) * self.stride() + usize::from(rect.left) * 4;
                if source.get(start..start + width * 4).is_none() || self.data.get(to..to + width * 4).is_none() {
                    return Err(custom_err!("native tile row", KernelError::Length));
                }
            }
        }
        #[cfg(test)]
        NATIVE_IMAGE_CALLS.with(|v| v.borrow_mut().push(("begin", 0)));
        let state = self.pointer_rendering_begin_with_kernels(&region.extents, kernels)?;
        let stride = self.stride();
        for rect in &region.rectangles {
            let x = usize::from(rect.left - update.left);
            let y = usize::from(rect.top - update.top);
            let width = usize::from(rect.right) - usize::from(rect.left) + 1;
            let height = usize::from(rect.bottom) - usize::from(rect.top) + 1;
            for row in 0..height {
                let start = (y + row) * 64 * 4 + x * 4;
                let to = (usize::from(rect.top) + row) * stride + usize::from(rect.left) * 4;
                #[cfg(test)]
                NATIVE_IMAGE_CALLS.with(|v| v.borrow_mut().push(("row", width * 4)));
                if let Some(kernels) = kernels {
                    kernels
                        .convert_pixel_row(
                            format,
                            destination_format,
                            &source[start..start + width * 4],
                            None,
                            &mut self.data[to..to + width * 4],
                        )
                        .map_err(|e| custom_err!("native tile row", e))?;
                } else {
                    // 复用原 copy_to 像素算法；单行切片避免 full-image u16 stride 溢出。
                    let row_width = u16::try_from(width).map_err(|e| custom_err!("tile 行", e))?;
                    let row_rect = InclusiveRectangle {
                        left: 0,
                        top: 0,
                        right: row_width - 1,
                        bottom: 0,
                    };
                    let from = ImageRegion {
                        region: row_rect.clone(),
                        data: &source[start..start + width * 4],
                        step: row_width * 4,
                        pixel_format: source_format,
                    };
                    let mut dest = ImageRegionMut {
                        region: row_rect,
                        data: &mut self.data[to..to + width * 4],
                        step: row_width * 4,
                        pixel_format: self.pixel_format,
                    };
                    from.copy_to(&mut dest)
                        .map_err(|e| custom_err!("精确 tile copy_to", e))?;
                }
            }
        }
        #[cfg(test)]
        NATIVE_IMAGE_CALLS.with(|v| v.borrow_mut().push(("end", 0)));
        self.pointer_rendering_end(state).map(Some)
    }

    pub(crate) fn apply_tile(
        &mut self,
        tile_output: &[u8],
        pixel_format: PixelFormat,
        clipping_rectangles: &Region,
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle> {
        trace!("Tile: {:?}", update_rectangle);

        if !self.rect_fits(&clipping_rectangles.extents) {
            debug!(
                "Skipping tile update {:?} outside image bounds {}x{}",
                clipping_rectangles.extents, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        let pointer_rendering_state = self.pointer_rendering_begin(&clipping_rectangles.extents)?;

        let update_region = clipping_rectangles.intersect_rectangle(update_rectangle);
        for region_rectangle in &update_region.rectangles {
            let source_x = region_rectangle.left - update_rectangle.left;
            let source_y = region_rectangle.top - update_rectangle.top;
            let stride = u16::from(pixel_format.bytes_per_pixel()) * TILE_SIZE;
            let source_image_region = ImageRegion {
                region: InclusiveRectangle {
                    left: source_x,
                    top: source_y,
                    right: source_x + region_rectangle.width() - 1,
                    bottom: source_y + region_rectangle.height() - 1,
                },
                data: tile_output,
                step: stride,
                pixel_format,
            };

            let mut destination_image_region = ImageRegionMut {
                region: region_rectangle.clone(),
                step: self.width() * u16::from(self.pixel_format.bytes_per_pixel()),
                pixel_format: self.pixel_format,
                data: &mut self.data,
            };

            trace!("Source image region: {:?}", source_image_region.region);
            trace!("Destination image region: {:?}", destination_image_region.region);

            source_image_region
                .copy_to(&mut destination_image_region)
                .map_err(|e| custom_err!("copy_to", e))?;
        }

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    pub(crate) fn apply_rgb16_bitmap(
        &mut self,
        rgb16: &[u8],
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle> {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping rgb16 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        const SRC_COLOR_DEPTH: usize = 2;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let rectangle_width = usize::from(update_rectangle.width());
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);
        let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        rgb16
            .chunks_exact(rectangle_width * SRC_COLOR_DEPTH)
            .rev()
            .enumerate()
            .for_each(|(row_idx, row)| {
                row.chunks_exact(SRC_COLOR_DEPTH)
                    .enumerate()
                    .for_each(|(col_idx, src_pixel)| {
                        let rgb16_value = u16::from_le_bytes(
                            src_pixel
                                .try_into()
                                .expect("src_pixel contains exactly two u8 elements"),
                        );
                        let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                        let [r, g, b] = rdp_16bit_to_rgb(rgb16_value);
                        self.data[dst_idx + ri] = r;
                        self.data[dst_idx + gi] = g;
                        self.data[dst_idx + bi] = b;
                        self.data[dst_idx + ai] = 0xff;
                    })
            });

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    /// Apply a 15-bit (RGB555) bitmap. Bottom-up row order, 2 bytes per pixel.
    pub(crate) fn apply_rgb15_bitmap(
        &mut self,
        rgb15: &[u8],
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle> {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping rgb15 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        const SRC_COLOR_DEPTH: usize = 2;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let rectangle_width = usize::from(update_rectangle.width());
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);
        let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        rgb15
            .chunks_exact(rectangle_width * SRC_COLOR_DEPTH)
            .rev()
            .enumerate()
            .for_each(|(row_idx, row)| {
                row.chunks_exact(SRC_COLOR_DEPTH)
                    .enumerate()
                    .for_each(|(col_idx, src_pixel)| {
                        let rgb15_value = u16::from_le_bytes(
                            src_pixel
                                .try_into()
                                .expect("src_pixel contains exactly two u8 elements"),
                        );
                        let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                        let [r, g, b] = rdp_15bit_to_rgb(rgb15_value);
                        self.data[dst_idx + ri] = r;
                        self.data[dst_idx + gi] = g;
                        self.data[dst_idx + bi] = b;
                        self.data[dst_idx + ai] = 0xff;
                    })
            });

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    /// Apply a 24-bit BGR bitmap. RLE 24bpp decompresses to BGR byte order,
    /// and uncompressed 24bpp bitmaps are also BGR per MS-RDPBCGR.
    /// Bottom-up row order, 3 bytes per pixel.
    pub(crate) fn apply_bgr24_bitmap(
        &mut self,
        bgr24: &[u8],
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle> {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping bgr24 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        const SRC_COLOR_DEPTH: usize = 3;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let rectangle_width = usize::from(update_rectangle.width());
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);
        let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        bgr24
            .chunks_exact(rectangle_width * SRC_COLOR_DEPTH)
            .rev()
            .enumerate()
            .for_each(|(row_idx, row)| {
                row.chunks_exact(SRC_COLOR_DEPTH)
                    .enumerate()
                    .for_each(|(col_idx, src_pixel)| {
                        let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                        // BGR -> RGB channel swap
                        self.data[dst_idx + ri] = src_pixel[2];
                        self.data[dst_idx + gi] = src_pixel[1];
                        self.data[dst_idx + bi] = src_pixel[0];
                        self.data[dst_idx + ai] = 0xff;
                    })
            });

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    /// Apply an 8-bit palette-indexed bitmap. Each source byte is a palette index.
    /// Bottom-up row order.
    pub(crate) fn apply_rgb8_with_palette(
        &mut self,
        indexed: &[u8],
        update_rectangle: &InclusiveRectangle,
        palette: &[[u8; 3]; 256],
    ) -> SessionResult<InclusiveRectangle> {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping rgb8 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let rectangle_width = usize::from(update_rectangle.width());
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);
        let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        indexed
            .chunks_exact(rectangle_width)
            .rev()
            .enumerate()
            .for_each(|(row_idx, row)| {
                row.iter().enumerate().for_each(|(col_idx, &index)| {
                    let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;
                    let [r, g, b] = palette[usize::from(index)];
                    self.data[dst_idx + ri] = r;
                    self.data[dst_idx + gi] = g;
                    self.data[dst_idx + bi] = b;
                    self.data[dst_idx + ai] = 0xff;
                })
            });

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    fn apply_rgb24_iter<'a, I>(
        &mut self,
        rgb24: I,
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle>
    where
        I: Iterator<Item = &'a [u8]>,
    {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping rgb24 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        const SRC_COLOR_DEPTH: usize = 3;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);
        let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        rgb24.enumerate().for_each(|(row_idx, row)| {
            row.chunks_exact(SRC_COLOR_DEPTH)
                .enumerate()
                .for_each(|(col_idx, src_pixel)| {
                    let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                    self.data[dst_idx + ri] = src_pixel[0];
                    self.data[dst_idx + gi] = src_pixel[1];
                    self.data[dst_idx + bi] = src_pixel[2];
                    self.data[dst_idx + ai] = 0xFF;
                })
        });

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    pub(crate) fn apply_rgb24(
        &mut self,
        rgb24: &[u8],
        update_rectangle: &InclusiveRectangle,
        flip: bool,
    ) -> SessionResult<InclusiveRectangle> {
        const SRC_COLOR_DEPTH: usize = 3;
        let rectangle_width = usize::from(update_rectangle.width());
        let lines = rgb24.chunks_exact(rectangle_width * SRC_COLOR_DEPTH);
        if flip {
            self.apply_rgb24_iter(lines.rev(), update_rectangle)
        } else {
            self.apply_rgb24_iter(lines, update_rectangle)
        }
    }

    #[cfg(feature = "qoi")]
    fn apply_rgba32_iter<'a, I>(
        &mut self,
        rgba32: I,
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle>
    where
        I: Iterator<Item = &'a [u8]>,
    {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping rgba32 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        const SRC_COLOR_DEPTH: usize = 4;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);
        let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        rgba32.enumerate().for_each(|(row_idx, row)| {
            row.chunks_exact(SRC_COLOR_DEPTH)
                .enumerate()
                .for_each(|(col_idx, src_pixel)| {
                    let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                    self.data[dst_idx + ri] = src_pixel[0];
                    self.data[dst_idx + gi] = src_pixel[1];
                    self.data[dst_idx + bi] = src_pixel[2];
                    self.data[dst_idx + ai] = src_pixel[3];
                })
        });

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    #[cfg(feature = "qoi")]
    pub(crate) fn apply_rgba32(
        &mut self,
        rgba32: &[u8],
        update_rectangle: &InclusiveRectangle,
        flip: bool,
    ) -> SessionResult<InclusiveRectangle> {
        const SRC_COLOR_DEPTH: usize = 4;
        let rectangle_width = usize::from(update_rectangle.width());
        let lines = rgba32.chunks_exact(rectangle_width * SRC_COLOR_DEPTH);
        if flip {
            self.apply_rgba32_iter(lines.rev(), update_rectangle)
        } else {
            self.apply_rgba32_iter(lines, update_rectangle)
        }
    }

    pub(crate) fn apply_rgb32_bitmap(
        &mut self,
        rgb32: &[u8],
        format: PixelFormat,
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle> {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping rgb32 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        const SRC_COLOR_DEPTH: usize = 4;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let rectangle_width = usize::from(update_rectangle.width());
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        if format == self.pixel_format {
            rgb32
                .chunks_exact(rectangle_width * SRC_COLOR_DEPTH)
                .rev()
                .enumerate()
                .for_each(|(row_idx, row)| {
                    row.chunks_exact(SRC_COLOR_DEPTH)
                        .enumerate()
                        .for_each(|(col_idx, src_pixel)| {
                            let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                            self.data[dst_idx..dst_idx + SRC_COLOR_DEPTH].copy_from_slice(src_pixel);
                        })
                });
        } else {
            let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();
            rgb32
                .chunks_exact(rectangle_width * SRC_COLOR_DEPTH)
                .rev()
                .enumerate()
                .try_for_each(|(row_idx, row)| {
                    row.chunks_exact(SRC_COLOR_DEPTH)
                        .enumerate()
                        .try_for_each(|(col_idx, src_pixel)| {
                            let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                            let c = format
                                .read_color(src_pixel)
                                .map_err(|err| custom_err!("read color", err))?;

                            self.data[dst_idx + ri] = c.r;
                            self.data[dst_idx + gi] = c.g;
                            self.data[dst_idx + bi] = c.b;
                            self.data[dst_idx + ai] = c.a;

                            Ok(())
                        })?;

                    Ok(())
                })?;
        }

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }
}

#[cfg(test)]
#[path = "../tests/cursor_image_consumers.rs"]
mod cursor_consumers;
