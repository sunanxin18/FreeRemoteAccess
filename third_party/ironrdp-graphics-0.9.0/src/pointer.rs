//! This module implements logic to decode pointer PDUs into RGBA bitmaps ready for rendering.
//!
//! # References:
//! - Drawing pointers: <https://learn.microsoft.com/en-us/windows-hardware/drivers/display/pointer-drawing>
//! - Drawing color pointers: <https://learn.microsoft.com/en-us/windows-hardware/drivers/display/drawing-color-pointers>
//! - Drawing monochrome pointers <https://learn.microsoft.com/en-us/windows-hardware/drivers/display/drawing-monochrome-pointers>
//!
//!
//! # Notes on xor/and masks encoding:
//! RDP's pointer representation is a bit weird. It uses two masks to represent a pointer -
//! andMask and xorMask. Xor mask is used as a base color for a pointer pixel, and andMask
//! mask is used co control pixel's full transparency (`src_color.a = 0`), full opacity
//! (`src_color.a = 255`) or pixel inversion (`dst_color.rgb = vec3(255) - dst_color.rgb`).
//!
//! Xor basks could be 1, 8, 16, 24 or 32 bits per pixel, and andMask is always 1 bit per pixel.
//!
//! Rules for decoding masks:
//! - `andMask == 0` -> dst_color Copy pixel from xorMask
//! - andMask == 1, xorMask == 0(black color) -> Transparent pixel
//! - andMask == 1, xorMask == 1(white color) -> Pixel is inverted

use ironrdp_core::ReadCursor;
use ironrdp_pdu::pointer::{ColorPointerAttribute, LargePointerAttribute, PointerAttribute};

use crate::color_conversion::rdp_16bit_to_rgb;
use crate::legacy_native::{CursorBitmapMode, CursorXorFormat, KernelError, NativeKernels};

const SUPPORTED_COLOR_BPP: [u16; 4] = [1, 16, 24, 32];

#[derive(Debug)]
pub enum PointerError {
    InvalidXorMaskSize { expected: usize, actual: usize },
    InvalidAndMaskSize { expected: usize, actual: usize },
    NotSupportedBpp { bpp: u16 },
    Pdu(ironrdp_pdu::PduError),
}

impl core::fmt::Display for PointerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PointerError::InvalidXorMaskSize { expected, actual } => {
                write!(
                    f,
                    "invalid pointer xorMask size. Expected: {expected}, actual: {actual}"
                )
            }
            PointerError::InvalidAndMaskSize { expected, actual } => {
                write!(
                    f,
                    "invalid pointer andMask size. Expected: {expected}, actual: {actual}"
                )
            }
            PointerError::NotSupportedBpp { bpp } => {
                write!(f, "not supported pointer bpp: {bpp}")
            }
            PointerError::Pdu(err) => err.fmt(f),
        }
    }
}

impl core::error::Error for PointerError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            PointerError::InvalidXorMaskSize { .. } => None,
            PointerError::InvalidAndMaskSize { .. } => None,
            PointerError::NotSupportedBpp { .. } => None,
            PointerError::Pdu(error) => error.source(),
        }
    }
}

impl From<ironrdp_pdu::PduError> for PointerError {
    fn from(error: ironrdp_pdu::PduError) -> Self {
        PointerError::Pdu(error)
    }
}

/// 显式原生 materialization 的错误；Compatibility 的公开错误类型保持原样。
#[derive(Debug)]
pub enum NativePointerError {
    Pointer(PointerError),
    Kernel(KernelError),
    Allocation(std::collections::TryReserveError),
}
impl core::fmt::Display for NativePointerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Pointer(e) => e.fmt(f),
            Self::Kernel(e) => e.fmt(f),
            Self::Allocation(e) => e.fmt(f),
        }
    }
}
impl core::error::Error for NativePointerError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Pointer(e) => Some(e),
            Self::Kernel(e) => Some(e),
            Self::Allocation(e) => Some(e),
        }
    }
}
impl From<PointerError> for NativePointerError {
    fn from(e: PointerError) -> Self {
        Self::Pointer(e)
    }
}
impl From<KernelError> for NativePointerError {
    fn from(e: KernelError) -> Self {
        Self::Kernel(e)
    }
}

/// Represents RDP pointer in decoded form (color channels stored as RGBA pre-multiplied values)
#[derive(Debug)]
pub struct DecodedPointer {
    pub width: u16,
    pub height: u16,
    pub hotspot_x: u16,
    pub hotspot_y: u16,
    pub bitmap_data: Vec<u8>,
}

/// Pointer bitmap rendering target. Defines properties and format of the decoded bitmap.
#[derive(Clone, Copy, Debug)]
pub enum PointerBitmapTarget {
    /// Software rendering target will produce RGBA bitmaps with premultiplied alpha.
    ///
    /// Colors with alpha channel set to 0x00 are always invisible no matter their color
    /// component. We could take advantage of that, and use a special color to represent
    /// inverted pixels. [0xFF, 0xFF, 0xFF, 0x00] is used for such purpose in software
    /// rendering mode.
    Software,
    /// Accelerated rendering target will produce RGBA bitmaps with non-premultiplied alpha.
    /// Inverted pixels will be rendered following the check pattern.
    Accelerated,
}

impl PointerBitmapTarget {
    fn should_premultiply_alpha(self) -> bool {
        match self {
            Self::Software => true,
            Self::Accelerated => false,
        }
    }

    fn should_invert_pixels_using_check_pattern(self) -> bool {
        match self {
            Self::Software => false,
            Self::Accelerated => true,
        }
    }
}

impl DecodedPointer {
    pub fn new_invisible() -> Self {
        Self {
            width: 0,
            height: 0,
            bitmap_data: Vec::new(),
            hotspot_x: 0,
            hotspot_y: 0,
        }
    }

    pub fn decode_pointer_attribute(
        src: &PointerAttribute<'_>,
        target: PointerBitmapTarget,
    ) -> Result<Self, PointerError> {
        Self::decode_pointer(PointerData::from(src), target)
    }

    /// 使用调用方保留的真实 ISA 能力；失败时不重试 Compatibility。
    pub fn decode_pointer_attribute_native(
        src: &PointerAttribute<'_>,
        target: PointerBitmapTarget,
        kernels: &NativeKernels,
    ) -> Result<Self, NativePointerError> {
        Self::decode_pointer_native(PointerData::from(src), target, kernels)
    }

    pub fn decode_color_pointer_attribute(
        src: &ColorPointerAttribute<'_>,
        target: PointerBitmapTarget,
    ) -> Result<Self, PointerError> {
        Self::decode_pointer(PointerData::from(src), target)
    }

    /// 使用调用方保留的真实 ISA 能力；失败时不重试 Compatibility。
    pub fn decode_color_pointer_attribute_native(
        src: &ColorPointerAttribute<'_>,
        target: PointerBitmapTarget,
        kernels: &NativeKernels,
    ) -> Result<Self, NativePointerError> {
        Self::decode_pointer_native(PointerData::from(src), target, kernels)
    }

    pub fn decode_large_pointer_attribute(
        src: &LargePointerAttribute<'_>,
        target: PointerBitmapTarget,
    ) -> Result<Self, PointerError> {
        Self::decode_pointer(PointerData::from(src), target)
    }

    /// 使用调用方保留的真实 ISA 能力；失败时不重试 Compatibility。
    pub fn decode_large_pointer_attribute_native(
        src: &LargePointerAttribute<'_>,
        target: PointerBitmapTarget,
        kernels: &NativeKernels,
    ) -> Result<Self, NativePointerError> {
        Self::decode_pointer_native(PointerData::from(src), target, kernels)
    }

    fn decode_pointer_native(
        data: PointerData<'_>,
        target: PointerBitmapTarget,
        kernels: &NativeKernels,
    ) -> Result<Self, NativePointerError> {
        // 直接 API 的空尺寸优先于 bpp/mask 检查，且清除 hotspot。
        if data.width == 0 || data.height == 0 {
            return Ok(Self::new_invisible());
        }
        data.validate_bpp()?;
        let width = usize::from(data.width);
        let height = usize::from(data.height);
        let xor_bits = width
            .checked_mul(usize::from(data.xor_bpp))
            .ok_or(KernelError::InvalidDimensions)?;
        let xor_stride = Stride::checked_from_bits(xor_bits)?;
        let and_stride = Stride::checked_from_bits(width)?;
        data.validate_xor(
            xor_stride
                .length
                .checked_mul(height)
                .ok_or(KernelError::InvalidDimensions)?,
        )?;
        data.validate_and(
            and_stride
                .length
                .checked_mul(height)
                .ok_or(KernelError::InvalidDimensions)?,
        )?;
        let (row_bytes, bytes) = checked_bitmap_size(width, height)?;
        let format = match data.xor_bpp {
            1 => CursorXorFormat::Mono1,
            16 => CursorXorFormat::Rgb565,
            24 => CursorXorFormat::Bgr24,
            32 => CursorXorFormat::Bgra32,
            _ => unreachable!("validated pointer bpp"),
        };
        let mut bitmap_data = Vec::new();
        bitmap_data
            .try_reserve_exact(bytes)
            .map_err(NativePointerError::Allocation)?;
        bitmap_data.resize(bytes, 0);
        for (row, out) in bitmap_data.chunks_exact_mut(row_bytes).enumerate() {
            let wire_row = if data.xor_bpp == 1 { row } else { height - row - 1 };
            let x = wire_row * xor_stride.length;
            let a = wire_row * and_stride.length;
            let and = if data.and_mask.is_empty() {
                None
            } else {
                Some(&data.and_mask[a..a + and_stride.data_bytes])
            };
            let mode = match target {
                PointerBitmapTarget::Software => CursorBitmapMode::Software,
                PointerBitmapTarget::Accelerated => CursorBitmapMode::Accelerated { odd_row: row % 2 != 0 },
            };
            // padding 不进入叶子；两种 mask 使用同一 wire 行，checker 仅取输出行奇偶。
            kernels.decode_cursor_row(format, mode, &data.xor_mask[x..x + xor_stride.data_bytes], and, out)?;
        }
        Ok(Self {
            width: data.width,
            height: data.height,
            hotspot_x: data.hot_spot_x,
            hotspot_y: data.hot_spot_y,
            bitmap_data,
        })
    }

    fn decode_pointer(data: PointerData<'_>, target: PointerBitmapTarget) -> Result<Self, PointerError> {
        if data.width == 0 || data.height == 0 {
            return Ok(Self::new_invisible());
        }

        data.validate_bpp()?;

        let flip_vertical = data.xor_bpp != 1;

        let and_stride = Stride::from_bits(data.width.into());
        let xor_stride = Stride::from_bits(usize::from(data.width) * usize::from(data.xor_bpp));

        data.validate_xor(xor_stride.length * usize::from(data.height))?;

        let default_and_mask = vec![0x00; and_stride.length * usize::from(data.height)];
        let mut and_mask = data.and_mask;
        if and_mask.is_empty() {
            and_mask = &default_and_mask;
        } else {
            data.validate_and(and_stride.length * usize::from(data.height))?;
        }

        let mut bitmap_data = Vec::new();

        for row_idx in 0..data.height {
            // For non-monochrome cursors we read strides from bottom to top
            let (mut xor_stride_cursor, mut and_stride_cursor) = if flip_vertical {
                let xor_stride_cursor =
                    ReadCursor::new(&data.xor_mask[usize::from(data.height - row_idx - 1) * xor_stride.length..]);
                let and_stride_cursor =
                    ReadCursor::new(&and_mask[usize::from(data.height - row_idx - 1) * and_stride.length..]);
                (xor_stride_cursor, and_stride_cursor)
            } else {
                let xor_stride_cursor = ReadCursor::new(&data.xor_mask[usize::from(row_idx) * xor_stride.length..]);
                let and_stride_cursor = ReadCursor::new(&and_mask[usize::from(row_idx) * and_stride.length..]);
                (xor_stride_cursor, and_stride_cursor)
            };

            let mut color_reader = ColorStrideReader::new(data.xor_bpp, xor_stride)?;
            let mut bitmask_reader = BitmaskStrideReader::new(and_stride);

            let compute_inverted_pixel = if target.should_invert_pixels_using_check_pattern() {
                |row_idx: u16, col_idx: u16| -> [u8; 4] {
                    // Checkered pattern is used to represent inverted pixels.
                    if (row_idx + col_idx).is_multiple_of(2) {
                        [0xff, 0xff, 0xff, 0xff]
                    } else {
                        [0x00, 0x00, 0x00, 0xff]
                    }
                }
            } else {
                |_, _| [0xFF, 0xFF, 0xFF, 0x00]
            };

            for col_idx in 0..data.width {
                let and_bit = bitmask_reader.next_bit(&mut and_stride_cursor);
                let color = color_reader.next_pixel(&mut xor_stride_cursor);

                if and_bit == 1 && color == [0, 0, 0, 0xff] {
                    // Force transparent pixel (The only way to get a transparent pixel with
                    // non-32-bit cursors)
                    bitmap_data.extend_from_slice(&[0, 0, 0, 0]);
                } else if and_bit == 1 && color == [0xff, 0xff, 0xff, 0xff] {
                    // Inverted pixel.
                    bitmap_data.extend_from_slice(&compute_inverted_pixel(row_idx, col_idx));
                } else if target.should_premultiply_alpha() {
                    // Calculate premultiplied alpha via integer arithmetic
                    let with_premultiplied_alpha = [
                        u8::try_from((u16::from(color[0]) * u16::from(color[0])) >> 8)
                            .expect("(u16 >> 8) fits into u8"),
                        u8::try_from((u16::from(color[1]) * u16::from(color[1])) >> 8)
                            .expect("(u16 >> 8) fits into u8"),
                        u8::try_from((u16::from(color[2]) * u16::from(color[2])) >> 8)
                            .expect("(u16 >> 8) fits into u8"),
                        color[3],
                    ];
                    bitmap_data.extend_from_slice(&with_premultiplied_alpha);
                } else {
                    bitmap_data.extend_from_slice(&color);
                }
            }
        }

        Ok(Self {
            width: data.width,
            height: data.height,
            bitmap_data,
            hotspot_x: data.hot_spot_x,
            hotspot_y: data.hot_spot_y,
        })
    }
}

// 256 MiB 是既有 legacy 资源策略，绝不是 pointer wire 尺寸上限。
fn checked_bitmap_size(width: usize, height: usize) -> Result<(usize, usize), KernelError> {
    let row_bytes = width.checked_mul(4).ok_or(KernelError::InvalidDimensions)?;
    let bytes = row_bytes.checked_mul(height).ok_or(KernelError::InvalidDimensions)?;
    if bytes > crate::rle::MAX_NATIVE_BITMAP_BYTES || bytes > isize::MAX as usize {
        return Err(KernelError::InvalidDimensions);
    }
    Ok((row_bytes, bytes))
}

#[derive(Clone, Copy)]
struct Stride {
    length: usize,
    data_bytes: usize,
    padding: usize,
}

impl Stride {
    fn checked_from_bits(bits: usize) -> Result<Self, KernelError> {
        let data_bytes = bits.checked_add(7).ok_or(KernelError::InvalidDimensions)? / 8;
        let length = (bits.checked_add(15).ok_or(KernelError::InvalidDimensions)? / 16)
            .checked_mul(2)
            .ok_or(KernelError::InvalidDimensions)?;
        Ok(Self {
            length,
            data_bytes,
            padding: length - data_bytes,
        })
    }
    fn from_bits(bits: usize) -> Stride {
        let length = bit_stride_size_align_u16(bits);
        let data_bytes = bit_stride_size_align_u8(bits);
        Stride {
            length,
            data_bytes,
            padding: length - data_bytes,
        }
    }
}

struct BitmaskStrideReader {
    current_byte: u8,
    read_bits: usize,
    read_stide_bytes: usize,
    stride_data_bytes: usize,
    stride_padding: usize,
}

impl BitmaskStrideReader {
    fn new(stride: Stride) -> Self {
        Self {
            current_byte: 0,
            read_bits: 8,
            read_stide_bytes: 0,
            stride_data_bytes: stride.data_bytes,
            stride_padding: stride.padding,
        }
    }

    fn next_bit(&mut self, cursor: &mut ReadCursor<'_>) -> u8 {
        if self.read_bits == 8 {
            self.read_bits = 0;

            if self.read_stide_bytes == self.stride_data_bytes {
                self.read_stide_bytes = 0;
                cursor.read_slice(self.stride_padding);
            }

            self.current_byte = cursor.read_u8();
        }

        let bit = (self.current_byte >> (7 - self.read_bits)) & 1;
        self.read_bits += 1;
        bit
    }
}

enum ColorStrideReader {
    Color {
        /// INVARIANT: `bpp == 16 || bpp == 24 || bpp == 32`
        bpp: u16,
        read_stide_bytes: usize,
        stride_data_bytes: usize,
        stride_padding: usize,
    },
    Bitmask(BitmaskStrideReader),
}

impl ColorStrideReader {
    fn new(bpp: u16, stride: Stride) -> Result<Self, PointerError> {
        Ok(match bpp {
            1 => Self::Bitmask(BitmaskStrideReader::new(stride)),
            bpp => Self::Color {
                bpp: {
                    // Enforce the bpp == 16 || bpp == 24 || bpp == 32 invariant.
                    if !SUPPORTED_COLOR_BPP[1..].contains(&bpp) {
                        return Err(PointerError::NotSupportedBpp { bpp });
                    }

                    bpp
                },
                read_stide_bytes: 0,
                stride_data_bytes: stride.data_bytes,
                stride_padding: stride.padding,
            },
        })
    }

    fn next_pixel(&mut self, cursor: &mut ReadCursor<'_>) -> [u8; 4] {
        match self {
            ColorStrideReader::Color {
                bpp,
                read_stide_bytes,
                stride_data_bytes,
                stride_padding,
            } => {
                if read_stide_bytes == stride_data_bytes {
                    *read_stide_bytes = 0;
                    cursor.read_slice(*stride_padding);
                }

                match bpp {
                    16 => {
                        *read_stide_bytes += 2;
                        let color_16bit = cursor.read_u16();
                        let [r, g, b] = rdp_16bit_to_rgb(color_16bit);
                        [r, g, b, 0xff]
                    }
                    24 => {
                        *read_stide_bytes += 3;

                        let color_24bit = cursor.read_array::<3>();
                        [color_24bit[2], color_24bit[1], color_24bit[0], 0xff]
                    }
                    32 => {
                        *read_stide_bytes += 4;
                        let color_32bit = cursor.read_array::<4>();
                        [color_32bit[2], color_32bit[1], color_32bit[0], color_32bit[3]]
                    }
                    _ => unreachable!("per the invariant on self.bpp, this path is unreachable"),
                }
            }
            ColorStrideReader::Bitmask(bitask) => {
                if bitask.next_bit(cursor) == 1 {
                    [0xff, 0xff, 0xff, 0xff]
                } else {
                    [0, 0, 0, 0xff]
                }
            }
        }
    }
}

fn bit_stride_size_align_u8(size_bits: usize) -> usize {
    size_bits.div_ceil(8)
}

fn bit_stride_size_align_u16(size_bits: usize) -> usize {
    size_bits.div_ceil(16) * 2
}

/// Message-agnostic pointer data.
struct PointerData<'a> {
    width: u16,
    height: u16,
    xor_bpp: u16,
    xor_mask: &'a [u8],
    and_mask: &'a [u8],
    hot_spot_x: u16,
    hot_spot_y: u16,
}

impl PointerData<'_> {
    fn validate_bpp(&self) -> Result<(), PointerError> {
        if !SUPPORTED_COLOR_BPP.contains(&self.xor_bpp) {
            return Err(PointerError::NotSupportedBpp { bpp: self.xor_bpp });
        }
        Ok(())
    }
    fn validate_xor(&self, expected: usize) -> Result<(), PointerError> {
        if self.xor_mask.len() != expected {
            return Err(PointerError::InvalidXorMaskSize {
                expected,
                actual: self.xor_mask.len(),
            });
        }
        Ok(())
    }
    fn validate_and(&self, expected: usize) -> Result<(), PointerError> {
        if !self.and_mask.is_empty() && self.and_mask.len() != expected {
            return Err(PointerError::InvalidAndMaskSize {
                expected,
                actual: self.and_mask.len(),
            });
        }
        Ok(())
    }
}
impl<'a> From<&PointerAttribute<'a>> for PointerData<'a> {
    fn from(src: &PointerAttribute<'a>) -> Self {
        Self {
            xor_bpp: src.xor_bpp,
            ..Self::from(&src.color_pointer)
        }
    }
}
impl<'a> From<&ColorPointerAttribute<'a>> for PointerData<'a> {
    fn from(src: &ColorPointerAttribute<'a>) -> Self {
        Self {
            width: src.width,
            height: src.height,
            xor_bpp: 24,
            xor_mask: src.xor_mask,
            and_mask: src.and_mask,
            hot_spot_x: src.hot_spot.x,
            hot_spot_y: src.hot_spot.y,
        }
    }
}
impl<'a> From<&LargePointerAttribute<'a>> for PointerData<'a> {
    fn from(src: &LargePointerAttribute<'a>) -> Self {
        Self {
            width: src.width,
            height: src.height,
            xor_bpp: src.xor_bpp,
            xor_mask: src.xor_mask,
            and_mask: src.and_mask,
            hot_spot_x: src.hot_spot.x,
            hot_spot_y: src.hot_spot.y,
        }
    }
}
#[cfg(test)]
#[path = "../tests/cursor_consumers.rs"]
mod cursor_consumers;
