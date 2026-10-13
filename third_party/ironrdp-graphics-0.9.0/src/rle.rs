//! Interleaved Run-Length Encoding (RLE) Bitmap Codec
//!
//! # References
//!
//! - Microsoft Learn:
//!   - [RLE_BITMAP_STREAM](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/b3b60873-16a8-4cbc-8aaa-5f0a93083280)
//!   - [Pseudo-code](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/b6a3f5c2-0804-4c10-9d25-a321720fd23e)
//!
//! - FreeRDP:
//!   - [interleaved.c](https://github.com/FreeRDP/FreeRDP/blob/db98f16e5bce003c898e8c85eb7af964f22a16a8/libfreerdp/codec/interleaved.c#L3)
//!   - [bitmap.c](https://github.com/FreeRDP/FreeRDP/blob/3a8dce07ea0262b240025bd68b63801578ca63f0/libfreerdp/codec/include/bitmap.c)
use core::fmt;
use core::ops::BitXor;

use frd_rdp_legacy_kernels::{KernelError, NativeKernels, PackedDepth};

#[cfg(test)]
#[path = "../tests/bitmap_wire.rs"]
mod bitmap_wire;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RlePixelFormat {
    Rgb24,
    Rgb16,
    Rgb15,
    Rgb8,
}

/// Decompresses an RLE compressed bitmap.
///
/// `src`: source buffer containing compressed bitmap
/// `dst`: destination buffer
/// `width`: decompressed bitmap width
/// `height`: decompressed bitmap height
/// `bpp`: bits per pixel
pub fn decompress(
    src: &[u8],
    dst: &mut Vec<u8>,
    width: usize,
    height: usize,
    bpp: usize,
) -> Result<RlePixelFormat, RleError> {
    match bpp {
        Mode24Bpp::BPP => decompress_24_bpp(src, dst, width, height),
        Mode16Bpp::BPP => decompress_16_bpp(src, dst, width, height),
        Mode15Bpp::BPP => decompress_15_bpp(src, dst, width, height),
        Mode8Bpp::BPP => decompress_8_bpp(src, dst, width, height),
        invalid => Err(RleError::InvalidBpp { bpp: invalid }),
    }
}

/// 返回这一次原标量解析实际写入和消费的字节数。
/// 成功欠填充仍为 `Ok`；预分配长度和旧缓冲区内容不是完成性证明。
pub fn decompress_observed(
    src: &[u8],
    dst: &mut Vec<u8>,
    width: usize,
    height: usize,
    bpp: usize,
) -> Result<(RlePixelFormat, RleDecodeProgress), RleError> {
    match bpp {
        Mode24Bpp::BPP => decompress_observed_helper::<Mode24Bpp>(src, dst, width, height),
        Mode16Bpp::BPP => decompress_observed_helper::<Mode16Bpp>(src, dst, width, height),
        Mode15Bpp::BPP => decompress_observed_helper::<Mode15Bpp>(src, dst, width, height),
        Mode8Bpp::BPP => decompress_observed_helper::<Mode8Bpp>(src, dst, width, height),
        invalid => Err(RleError::InvalidBpp { bpp: invalid }),
    }
}

/// 原版 wire 错误与 checked native 叶子错误保持可区分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeRleError {
    Rle(RleError),
    Kernel(KernelError),
}

impl From<RleError> for NativeRleError {
    fn from(error: RleError) -> Self {
        Self::Rle(error)
    }
}
impl From<KernelError> for NativeRleError {
    fn from(error: KernelError) -> Self {
        Self::Kernel(error)
    }
}
impl fmt::Display for NativeRleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rle(error) => error.fmt(f),
            Self::Kernel(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for NativeRleError {}

/// 显式选择 checked SIMD materialization；默认入口保持原版行为。
/// 尺寸溢出在 resize 前返回错误，wire 错误保留原版优先序与已写入的 dst 前缀。
pub fn decompress_native(
    src: &[u8],
    dst: &mut Vec<u8>,
    width: usize,
    height: usize,
    bpp: usize,
    kernels: &NativeKernels,
) -> Result<RlePixelFormat, NativeRleError> {
    match bpp {
        Mode24Bpp::BPP => decompress_native_helper::<Mode24Bpp>(src, dst, width, height, kernels),
        Mode16Bpp::BPP => decompress_native_helper::<Mode16Bpp>(src, dst, width, height, kernels),
        Mode15Bpp::BPP => decompress_native_helper::<Mode15Bpp>(src, dst, width, height, kernels),
        Mode8Bpp::BPP => decompress_native_helper::<Mode8Bpp>(src, dst, width, height, kernels),
        bpp => Err(RleError::InvalidBpp { bpp }.into()),
    }
}

fn decompress_native_helper<Mode: DepthMode>(
    src: &[u8],
    dst: &mut Vec<u8>,
    width: usize,
    height: usize,
    kernels: &NativeKernels,
) -> Result<RlePixelFormat, NativeRleError> {
    if width == 0 || height == 0 {
        return Err(RleError::EmptyImage.into());
    }
    let row_delta = Mode::COLOR_DEPTH
        .checked_mul(width)
        .ok_or(KernelError::InvalidDimensions)?;
    let size = row_delta
        .checked_mul(height)
        .filter(|&size| size <= isize::MAX as usize)
        .ok_or(KernelError::InvalidDimensions)?;
    dst.resize(size, 0);
    decompress_with::<Mode, _>(src, dst, row_delta, &mut NativeMaterializer::new(kernels))?;
    Ok(Mode::PIXEL_FORMAT)
}

/// 原生私有位图解码资源上限；完整源尺寸受限，裁剪不能绕过。
pub const MAX_NATIVE_BITMAP_BYTES: usize = 256 * 1024 * 1024;

/// 严格完整消费错误；不足写入发生在私有解码缓冲区，不能发布为图像区域。
#[derive(Debug)]
pub enum CompleteRleError {
    Native(NativeRleError),
    Incomplete {
        expected: usize,
        written: usize,
        consumed: usize,
    },
    Allocation(std::collections::TryReserveError),
}
impl fmt::Display for CompleteRleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(e) => e.fmt(f),
            Self::Incomplete {
                expected,
                written,
                consumed,
            } => write!(
                f,
                "RLE 解码不完整：写入 {written}/{expected} 字节，消费 {consumed} 字节"
            ),
            Self::Allocation(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for CompleteRleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Native(e) => Some(e),
            Self::Allocation(e) => Some(e),
            Self::Incomplete { .. } => None,
        }
    }
}
impl From<NativeRleError> for CompleteRleError {
    fn from(e: NativeRleError) -> Self {
        Self::Native(e)
    }
}

/// 同一 wire parser 的实际写入计数；长度不等于完整解码证据。不重试 scalar。
pub fn decompress_native_complete(
    src: &[u8],
    dst: &mut Vec<u8>,
    width: usize,
    height: usize,
    bpp: usize,
    kernels: &NativeKernels,
) -> Result<RlePixelFormat, CompleteRleError> {
    match bpp {
        24 => decompress_native_complete_helper::<Mode24Bpp>(src, dst, width, height, kernels),
        16 => decompress_native_complete_helper::<Mode16Bpp>(src, dst, width, height, kernels),
        15 => decompress_native_complete_helper::<Mode15Bpp>(src, dst, width, height, kernels),
        8 => decompress_native_complete_helper::<Mode8Bpp>(src, dst, width, height, kernels),
        bpp => Err(NativeRleError::Rle(RleError::InvalidBpp { bpp }).into()),
    }
}
fn decompress_native_complete_helper<Mode: DepthMode>(
    src: &[u8],
    dst: &mut Vec<u8>,
    width: usize,
    height: usize,
    kernels: &NativeKernels,
) -> Result<RlePixelFormat, CompleteRleError> {
    if width == 0 || height == 0 {
        return Err(NativeRleError::Rle(RleError::EmptyImage).into());
    }
    let row_delta = Mode::COLOR_DEPTH
        .checked_mul(width)
        .ok_or(NativeRleError::Kernel(KernelError::InvalidDimensions))?;
    let size = row_delta
        .checked_mul(height)
        .filter(|&n| n <= isize::MAX as usize && n <= MAX_NATIVE_BITMAP_BYTES)
        .ok_or(NativeRleError::Kernel(KernelError::InvalidDimensions))?;
    dst.try_reserve(size.saturating_sub(dst.len()))
        .map_err(CompleteRleError::Allocation)?;
    dst.resize(size, 0);
    let progress = decompress_with_progress::<Mode, _>(
        src,
        dst,
        row_delta,
        &mut NativeMaterializer::new(kernels),
    )?;
    if progress.written != size {
        return Err(CompleteRleError::Incomplete {
            expected: size,
            written: progress.written,
            consumed: progress.consumed,
        });
    }
    Ok(Mode::PIXEL_FORMAT)
}

/// Decompresses a 24-bpp RLE compressed bitmap.
///
/// `src`: source buffer containing compressed bitmap
/// `dst`: destination buffer
/// `width`: decompressed bitmap width
/// `height`: decompressed bitmap height
pub fn decompress_24_bpp(
    src: &[u8],
    dst: &mut Vec<u8>,
    width: usize,
    height: usize,
) -> Result<RlePixelFormat, RleError> {
    decompress_helper::<Mode24Bpp>(src, dst, width, height)
}

/// Decompresses a 16-bpp RLE compressed bitmap.
///
/// `src`: source buffer containing compressed bitmap
/// `dst`: destination buffer
/// `width`: decompressed bitmap width
/// `height`: decompressed bitmap height
pub fn decompress_16_bpp(
    src: &[u8],
    dst: &mut Vec<u8>,
    width: usize,
    height: usize,
) -> Result<RlePixelFormat, RleError> {
    decompress_helper::<Mode16Bpp>(src, dst, width, height)
}

/// Decompresses a 15-bpp RLE compressed bitmap.
///
/// `src`: source buffer containing compressed bitmap
/// `dst`: destination buffer
/// `width`: decompressed bitmap width
/// `height`: decompressed bitmap height
pub fn decompress_15_bpp(
    src: &[u8],
    dst: &mut Vec<u8>,
    width: usize,
    height: usize,
) -> Result<RlePixelFormat, RleError> {
    decompress_helper::<Mode15Bpp>(src, dst, width, height)
}

/// Decompresses a 8-bpp RLE compressed bitmap.
///
/// `src`: source buffer containing compressed bitmap
/// `dst`: destination buffer
/// `width`: decompressed bitmap width
/// `height`: decompressed bitmap height
pub fn decompress_8_bpp(
    src: &[u8],
    dst: &mut Vec<u8>,
    width: usize,
    height: usize,
) -> Result<RlePixelFormat, RleError> {
    decompress_helper::<Mode8Bpp>(src, dst, width, height)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RleError {
    InvalidBpp {
        bpp: usize,
    },
    BadOrderCode,
    NotEnoughBytes {
        expected: usize,
        actual: usize,
    },
    InvalidImageSize {
        maximum_additional: usize,
        required_additional: usize,
    },
    EmptyImage,
    UnexpectedZeroLength,
}

impl fmt::Display for RleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RleError::InvalidBpp { bpp } => {
                write!(f, "invalid bytes per pixel: {bpp}")
            }
            RleError::BadOrderCode => write!(f, "bad RLE order code"),
            RleError::NotEnoughBytes { expected, actual } => {
                write!(
                    f,
                    "not enough bytes: expected {expected} bytes, but got {actual}"
                )
            }
            RleError::InvalidImageSize {
                maximum_additional,
                required_additional,
            } => {
                write!(
                    f,
                    "invalid image size advertised: output buffer can only receive at most {maximum_additional} additional bytes, but {required_additional} bytes are required"
                )
            }
            RleError::EmptyImage => write!(f, "height or width is zero"),
            RleError::UnexpectedZeroLength => {
                write!(f, "unexpected zero-length")
            }
        }
    }
}

fn decompress_helper<Mode: DepthMode>(
    src: &[u8],
    dst: &mut Vec<u8>,
    width: usize,
    height: usize,
) -> Result<RlePixelFormat, RleError> {
    decompress_observed_helper::<Mode>(src, dst, width, height).map(|(format, _)| format)
}

fn decompress_observed_helper<Mode: DepthMode>(
    src: &[u8],
    dst: &mut Vec<u8>,
    width: usize,
    height: usize,
) -> Result<(RlePixelFormat, RleDecodeProgress), RleError> {
    if width == 0 || height == 0 {
        return Err(RleError::EmptyImage);
    }

    let row_delta = Mode::COLOR_DEPTH * width;
    dst.resize(row_delta * height, 0);
    let progress = decompress_impl::<Mode>(src, dst, row_delta)?;

    Ok((Mode::PIXEL_FORMAT, progress))
}

macro_rules! ensure_size {
    (from: $buf:ident, size: $expected:expr) => {{
        let actual = $buf.remaining_len();
        let expected = $expected;
        if expected > actual {
            return Err(RleError::NotEnoughBytes { expected, actual }.into());
        }
    }};
    (into: $buf:ident, size: $required_additional:expr) => {{
        let maximum_additional = $buf.remaining_len();
        let required_additional = $required_additional;
        if required_additional > maximum_additional {
            return Err(RleError::InvalidImageSize {
                maximum_additional,
                required_additional,
            }
            .into());
        }
    }};
}

/// RLE decompression implementation
///
/// `src`: source buffer containing compressed bitmap
/// `dst`: destination buffer
/// `row_delta`: scanline length in bytes
fn decompress_impl<Mode: DepthMode>(
    src: &[u8],
    dst: &mut [u8],
    row_delta: usize,
) -> Result<RleDecodeProgress, RleError> {
    decompress_with_progress::<Mode, _>(src, dst, row_delta, &mut ScalarMaterializer)
}

// 解析、命令级 first-line 状态、源消费及错误检查只有这一份实现。
fn decompress_with<Mode: DepthMode, M: Materializer<Mode>>(
    src: &[u8],
    dst: &mut [u8],
    row_delta: usize,
    materializer: &mut M,
) -> Result<(), M::Error> {
    decompress_with_progress::<Mode, M>(src, dst, row_delta, materializer).map(|_| ())
}

/// 单次成功解析的真实游标事实；不证明其他缓冲区或整幅图像已经完成。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RleDecodeProgress {
    /// 实际连续写入目标前缀的字节数（15bpp 每像素仍为两字节）。
    pub written: usize,
    /// 实际消费源流的字节数。
    pub consumed: usize,
}

fn decompress_with_progress<Mode: DepthMode, M: Materializer<Mode>>(
    src: &[u8],
    dst: &mut [u8],
    row_delta: usize,
    materializer: &mut M,
) -> Result<RleDecodeProgress, M::Error> {
    let mut src = Buf::new(src);
    let mut dst = BufMut::new(dst);

    let mut fg_pel = Mode::WHITE_PIXEL;
    let mut insert_fg_pel = false;
    let mut is_first_line = true;

    while !src.eof() {
        // Watch out for the end of the first scanline.
        if is_first_line && dst.pos >= row_delta {
            is_first_line = false;
            insert_fg_pel = false;
        }

        ensure_size!(from: src, size: 1);

        let header = src.read_u8();

        // Extract the compression order code ID from the compression order header.
        let code = Code::decode(header);

        // Extract run length
        let run_length = code.extract_run_length(header, &mut src)?;

        // Handle Background Run Orders.
        if code == Code::REGULAR_BG_RUN || code == Code::MEGA_MEGA_BG_RUN {
            ensure_size!(into: dst, size: run_length * Mode::COLOR_DEPTH);

            if insert_fg_pel {
                materializer.background(&mut dst, row_delta, is_first_line, Some(fg_pel), 1)?;
            }
            materializer.background(
                &mut dst,
                row_delta,
                is_first_line,
                None,
                run_length - usize::from(insert_fg_pel),
            )?;

            // A follow-on background run order will need a foreground pel inserted.
            insert_fg_pel = true;

            continue;
        }

        // For any of the other run-types a follow-on background run
        // order does not need a foreground pel inserted.
        insert_fg_pel = false;

        if code == Code::REGULAR_FG_RUN
            || code == Code::MEGA_MEGA_FG_RUN
            || code == Code::LITE_SET_FG_FG_RUN
            || code == Code::MEGA_MEGA_SET_FG_RUN
        {
            // Handle Foreground Run Orders.

            ensure_size!(from: src, size: Mode::COLOR_DEPTH);

            if code == Code::LITE_SET_FG_FG_RUN || code == Code::MEGA_MEGA_SET_FG_RUN {
                fg_pel = Mode::read_pixel(&mut src);
            }

            ensure_size!(into: dst, size: run_length * Mode::COLOR_DEPTH);

            materializer.background(
                &mut dst,
                row_delta,
                is_first_line,
                Some(fg_pel),
                run_length,
            )?;
        } else if code == Code::LITE_DITHERED_RUN || code == Code::MEGA_MEGA_DITHERED_RUN {
            // Handle Dithered Run Orders.

            ensure_size!(from: src, size: 2 * Mode::COLOR_DEPTH);

            let pixel_a = Mode::read_pixel(&mut src);
            let pixel_b = Mode::read_pixel(&mut src);

            ensure_size!(into: dst, size: run_length * 2 * Mode::COLOR_DEPTH);

            materializer.dither(&mut dst, pixel_a, pixel_b, run_length * 2)?;
        } else if code == Code::REGULAR_COLOR_RUN || code == Code::MEGA_MEGA_COLOR_RUN {
            // Handle Color Run Orders.

            ensure_size!(from: src, size: Mode::COLOR_DEPTH);

            let pixel = Mode::read_pixel(&mut src);

            ensure_size!(into: dst, size: run_length * Mode::COLOR_DEPTH);

            materializer.fill(&mut dst, pixel, run_length)?;
        } else if code == Code::REGULAR_FGBG_IMAGE
            || code == Code::MEGA_MEGA_FGBG_IMAGE
            || code == Code::LITE_SET_FG_FGBG_IMAGE
            || code == Code::MEGA_MEGA_SET_FGBG_IMAGE
        {
            // Handle Foreground/Background Image Orders.

            if code == Code::LITE_SET_FG_FGBG_IMAGE || code == Code::MEGA_MEGA_SET_FGBG_IMAGE {
                ensure_size!(from: src, size: Mode::COLOR_DEPTH);
                fg_pel = Mode::read_pixel(&mut src);
            }

            let mut number_to_read = run_length;

            while number_to_read > 0 {
                let c_bits = core::cmp::min(8, number_to_read);

                ensure_size!(from: src, size: 1);
                let bitmask = src.read_u8();

                ensure_size!(into: dst, size: c_bits * Mode::COLOR_DEPTH);
                materializer.mask(&mut dst, row_delta, is_first_line, bitmask, fg_pel, c_bits)?;

                number_to_read -= c_bits;
            }
        } else if code == Code::REGULAR_COLOR_IMAGE || code == Code::MEGA_MEGA_COLOR_IMAGE {
            // Handle Color Image Orders.

            let byte_count = run_length * Mode::COLOR_DEPTH;

            ensure_size!(from: src, size: byte_count);
            ensure_size!(into: dst, size: byte_count);

            materializer.copy(&mut dst, &src.inner[src.pos..src.pos + byte_count])?;
            src.pos += byte_count;
        } else if code == Code::SPECIAL_FGBG_1 {
            // Handle Special Order 1.

            const MASK_SPECIAL_FG_BG_1: u8 = 0x03;

            ensure_size!(into: dst, size: 8 * Mode::COLOR_DEPTH);
            materializer.mask(
                &mut dst,
                row_delta,
                is_first_line,
                MASK_SPECIAL_FG_BG_1,
                fg_pel,
                8,
            )?;
        } else if code == Code::SPECIAL_FGBG_2 {
            // Handle Special Order 2.

            const MASK_SPECIAL_FG_BG_2: u8 = 0x05;

            ensure_size!(into: dst, size: 8 * Mode::COLOR_DEPTH);
            materializer.mask(
                &mut dst,
                row_delta,
                is_first_line,
                MASK_SPECIAL_FG_BG_2,
                fg_pel,
                8,
            )?;
        } else if code == Code::SPECIAL_WHITE {
            // Handle White Order.

            ensure_size!(into: dst, size: Mode::COLOR_DEPTH);

            materializer.fill(&mut dst, Mode::WHITE_PIXEL, 1)?;
        } else if code == Code::SPECIAL_BLACK {
            // Handle Black Order.

            ensure_size!(into: dst, size: Mode::COLOR_DEPTH);

            materializer.fill(&mut dst, Mode::BLACK_PIXEL, 1)?;
        } else {
            return Err(RleError::BadOrderCode.into());
        }
    }

    Ok(RleDecodeProgress {
        written: dst.pos,
        consumed: src.pos,
    })
}

// Materializer 不决定 wire 检查或命令状态；调用前由共享 parser 保证 dst 空间。
trait Materializer<Mode: DepthMode> {
    type Error: From<RleError>;
    fn fill(
        &mut self,
        dst: &mut BufMut<'_>,
        pixel: Mode::Pixel,
        count: usize,
    ) -> Result<(), Self::Error>;
    fn background(
        &mut self,
        dst: &mut BufMut<'_>,
        row_delta: usize,
        first: bool,
        fg: Option<Mode::Pixel>,
        count: usize,
    ) -> Result<(), Self::Error>;
    fn dither(
        &mut self,
        dst: &mut BufMut<'_>,
        a: Mode::Pixel,
        b: Mode::Pixel,
        count: usize,
    ) -> Result<(), Self::Error>;
    fn mask(
        &mut self,
        dst: &mut BufMut<'_>,
        row_delta: usize,
        first: bool,
        mask: u8,
        fg: Mode::Pixel,
        count: usize,
    ) -> Result<(), Self::Error>;
    fn copy(&mut self, dst: &mut BufMut<'_>, src: &[u8]) -> Result<(), Self::Error>;
}

struct ScalarMaterializer;

impl<Mode: DepthMode> Materializer<Mode> for ScalarMaterializer {
    type Error = RleError;
    fn fill(
        &mut self,
        dst: &mut BufMut<'_>,
        pixel: Mode::Pixel,
        count: usize,
    ) -> Result<(), RleError> {
        for _ in 0..count {
            Mode::write_pixel(dst, pixel);
        }
        Ok(())
    }
    fn background(
        &mut self,
        dst: &mut BufMut<'_>,
        row_delta: usize,
        first: bool,
        fg: Option<Mode::Pixel>,
        count: usize,
    ) -> Result<(), RleError> {
        for _ in 0..count {
            let pixel = if first {
                fg.unwrap_or(Mode::BLACK_PIXEL)
            } else {
                let above = dst.read_pixel_above::<Mode>(row_delta);
                fg.map_or(above, |fg| above ^ fg)
            };
            Mode::write_pixel(dst, pixel);
        }
        Ok(())
    }
    fn dither(
        &mut self,
        dst: &mut BufMut<'_>,
        a: Mode::Pixel,
        b: Mode::Pixel,
        count: usize,
    ) -> Result<(), RleError> {
        for i in 0..count {
            Mode::write_pixel(dst, if i % 2 == 0 { a } else { b });
        }
        Ok(())
    }
    fn mask(
        &mut self,
        dst: &mut BufMut<'_>,
        row_delta: usize,
        first: bool,
        mask: u8,
        fg: Mode::Pixel,
        count: usize,
    ) -> Result<(), RleError> {
        if first {
            write_first_line_fg_bg_image::<Mode>(dst, mask, fg, count)
        } else {
            write_fg_bg_image::<Mode>(dst, row_delta, mask, fg, count)
        }
    }
    fn copy(&mut self, dst: &mut BufMut<'_>, src: &[u8]) -> Result<(), RleError> {
        for &byte in src {
            dst.write_u8(byte);
        }
        Ok(())
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
struct LeafCalls {
    fill: usize,
    copy: usize,
    xor: usize,
    mask: usize,
    dither: usize,
}

struct NativeMaterializer<'a> {
    kernels: &'a NativeKernels,
    #[cfg(test)]
    calls: LeafCalls,
}

impl<'a> NativeMaterializer<'a> {
    fn new(kernels: &'a NativeKernels) -> Self {
        Self {
            kernels,
            #[cfg(test)]
            calls: LeafCalls::default(),
        }
    }
    fn depth<Mode: DepthMode>() -> PackedDepth {
        match Mode::BPP {
            8 => PackedDepth::Index8,
            15 => PackedDepth::Rgb555,
            16 => PackedDepth::Rgb565,
            24 => PackedDepth::Bgr24,
            _ => unreachable!("内部固定 DepthMode"),
        }
    }
}

impl<Mode: DepthMode> Materializer<Mode> for NativeMaterializer<'_> {
    type Error = NativeRleError;
    fn fill(
        &mut self,
        dst: &mut BufMut<'_>,
        pixel: Mode::Pixel,
        count: usize,
    ) -> Result<(), NativeRleError> {
        let bytes = Into::<u32>::into(pixel).to_le_bytes();
        let len = count * Mode::COLOR_DEPTH;
        self.kernels.fill_pixel(
            Self::depth::<Mode>(),
            &bytes[..Mode::COLOR_DEPTH],
            &mut dst.inner[dst.pos..dst.pos + len],
        )?;
        #[cfg(test)]
        {
            self.calls.fill += 1;
        }
        dst.pos += len;
        Ok(())
    }
    fn background(
        &mut self,
        dst: &mut BufMut<'_>,
        row_delta: usize,
        first: bool,
        fg: Option<Mode::Pixel>,
        mut count: usize,
    ) -> Result<(), NativeRleError> {
        // first 是整个命令的判定；跨行不将它重新解释为当前 row。
        if first {
            return <Self as Materializer<Mode>>::fill(
                self,
                dst,
                fg.unwrap_or(Mode::BLACK_PIXEL),
                count,
            );
        }
        let fg_bytes = fg.map(|pixel| Into::<u32>::into(pixel).to_le_bytes());
        while count != 0 {
            let pixels = count.min((row_delta - dst.pos % row_delta) / Mode::COLOR_DEPTH);
            let len = pixels * Mode::COLOR_DEPTH;
            // 当前段最多一行；上一行已写数据与当前写入区域不重叠。
            let (before, current) = dst.inner.split_at_mut(dst.pos);
            let previous = &before[dst.pos - row_delta..dst.pos - row_delta + len];
            if let Some(bytes) = fg_bytes {
                self.kernels.xor_previous(
                    Self::depth::<Mode>(),
                    previous,
                    &bytes[..Mode::COLOR_DEPTH],
                    &mut current[..len],
                )?;
                #[cfg(test)]
                {
                    self.calls.xor += 1;
                }
            } else {
                self.kernels.copy_u8(previous, &mut current[..len])?;
                #[cfg(test)]
                {
                    self.calls.copy += 1;
                }
            }
            dst.pos += len;
            count -= pixels;
        }
        Ok(())
    }
    fn dither(
        &mut self,
        dst: &mut BufMut<'_>,
        a: Mode::Pixel,
        b: Mode::Pixel,
        count: usize,
    ) -> Result<(), NativeRleError> {
        let a = Into::<u32>::into(a).to_le_bytes();
        let b = Into::<u32>::into(b).to_le_bytes();
        let len = count * Mode::COLOR_DEPTH;
        // 无上一行依赖，单次 leaf 保留整个命令的 pair 相位，短尾由 leaf staging。
        self.kernels.dither_pixels(
            Self::depth::<Mode>(),
            &a[..Mode::COLOR_DEPTH],
            &b[..Mode::COLOR_DEPTH],
            false,
            &mut dst.inner[dst.pos..dst.pos + len],
        )?;
        #[cfg(test)]
        {
            self.calls.dither += 1;
        }
        dst.pos += len;
        Ok(())
    }
    fn mask(
        &mut self,
        dst: &mut BufMut<'_>,
        row_delta: usize,
        first: bool,
        mask: u8,
        fg: Mode::Pixel,
        mut count: usize,
    ) -> Result<(), NativeRleError> {
        let fg = Into::<u32>::into(fg).to_le_bytes();
        let mut bit_offset = 0;
        while count != 0 {
            let pixels = if first {
                count
            } else {
                count.min((row_delta - dst.pos % row_delta) / Mode::COLOR_DEPTH)
            };
            let len = pixels * Mode::COLOR_DEPTH;
            let (before, current) = dst.inner.split_at_mut(dst.pos);
            let previous = if first {
                None
            } else {
                Some(&before[dst.pos - row_delta..dst.pos - row_delta + len])
            };
            self.kernels.mask_previous(
                Self::depth::<Mode>(),
                previous,
                &fg[..Mode::COLOR_DEPTH],
                mask,
                bit_offset,
                &mut current[..len],
            )?;
            #[cfg(test)]
            {
                self.calls.mask += 1;
            }
            dst.pos += len;
            count -= pixels;
            // 一个 mask 只覆盖最多 8 个 pixel；row 边界不能重置 mask 位。
            bit_offset += u8::try_from(pixels).expect("mask 至多 8 pixels");
        }
        Ok(())
    }
    fn copy(&mut self, dst: &mut BufMut<'_>, src: &[u8]) -> Result<(), NativeRleError> {
        self.kernels
            .copy_u8(src, &mut dst.inner[dst.pos..dst.pos + src.len()])?;
        #[cfg(test)]
        {
            self.calls.copy += 1;
        }
        dst.pos += src.len();
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Code(u8);

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match *self {
            Self::REGULAR_BG_RUN => "REGULAR_BG_RUN",
            Self::REGULAR_FG_RUN => "REGULAR_FG_RUN",
            Self::REGULAR_COLOR_RUN => "REGULAR_COLOR_RUN",
            Self::REGULAR_FGBG_IMAGE => "REGULAR_FGBG_IMAGE",
            Self::REGULAR_COLOR_IMAGE => "REGULAR_COLOR_IMAGE",

            Self::MEGA_MEGA_BG_RUN => "MEGA_MEGA_BG_RUN",
            Self::MEGA_MEGA_FG_RUN => "MEGA_MEGA_FG_RUN",
            Self::MEGA_MEGA_SET_FG_RUN => "MEGA_MEGA_SET_FG_RUN",
            Self::MEGA_MEGA_DITHERED_RUN => "MEGA_MEGA_DITHERED_RUN",
            Self::MEGA_MEGA_COLOR_RUN => "MEGA_MEGA_COLOR_RUN",
            Self::MEGA_MEGA_FGBG_IMAGE => "MEGA_MEGA_FGBG_IMAGE",
            Self::MEGA_MEGA_SET_FGBG_IMAGE => "MEGA_MEGA_SET_FGBG_IMAGE",
            Self::MEGA_MEGA_COLOR_IMAGE => "MEGA_MEGA_COLOR_IMAGE",

            Self::LITE_SET_FG_FG_RUN => "LITE_SET_FG_FG_RUN",
            Self::LITE_DITHERED_RUN => "LITE_DITHERED_RUN",
            Self::LITE_SET_FG_FGBG_IMAGE => "LITE_SET_FG_FGBG_IMAGE",

            Self::SPECIAL_FGBG_1 => "SPECIAL_FGBG_1",
            Self::SPECIAL_FGBG_2 => "SPECIAL_FGBG_2",
            Self::SPECIAL_WHITE => "SPECIAL_WHITE",
            Self::SPECIAL_BLACK => "SPECIAL_BLACK",

            _ => "UNKNOWN",
        };

        write!(f, "Code(0x{:02X}-{name})", self.0)
    }
}

impl Code {
    const REGULAR_BG_RUN: Code = Code(0x00);
    const REGULAR_FG_RUN: Code = Code(0x01);
    const REGULAR_COLOR_RUN: Code = Code(0x03);
    const REGULAR_FGBG_IMAGE: Code = Code(0x02);
    const REGULAR_COLOR_IMAGE: Code = Code(0x04);

    const MEGA_MEGA_BG_RUN: Code = Code(0xF0);
    const MEGA_MEGA_FG_RUN: Code = Code(0xF1);
    const MEGA_MEGA_SET_FG_RUN: Code = Code(0xF6);
    const MEGA_MEGA_DITHERED_RUN: Code = Code(0xF8);
    const MEGA_MEGA_COLOR_RUN: Code = Code(0xF3);
    const MEGA_MEGA_FGBG_IMAGE: Code = Code(0xF2);
    const MEGA_MEGA_SET_FGBG_IMAGE: Code = Code(0xF7);
    const MEGA_MEGA_COLOR_IMAGE: Code = Code(0xF4);

    const LITE_SET_FG_FG_RUN: Code = Code(0x0C);
    const LITE_DITHERED_RUN: Code = Code(0x0E);
    const LITE_SET_FG_FGBG_IMAGE: Code = Code(0x0D);

    const SPECIAL_FGBG_1: Code = Code(0xF9);
    const SPECIAL_FGBG_2: Code = Code(0xFA);
    const SPECIAL_WHITE: Code = Code(0xFD);
    const SPECIAL_BLACK: Code = Code(0xFE);

    fn decode(header: u8) -> Self {
        if (header & 0xC0) != 0xC0 {
            // REGULAR orders
            // (000x xxxx, 001x xxxx, 010x xxxx, 011x xxxx, 100x xxxx)
            Code(header >> 5)
        } else if (header & 0xF0) == 0xF0 {
            // MEGA and SPECIAL orders (0xF*)
            Code(header)
        } else {
            // LITE orders
            // (1100 xxxx, 1101 xxxx, 1110 xxxx)
            Code(header >> 4)
        }
    }

    /// Extract the run length of a compression order.
    fn extract_run_length(self, header: u8, src: &mut Buf<'_>) -> Result<usize, RleError> {
        match self {
            Self::REGULAR_FGBG_IMAGE => {
                extract_run_length_fg_bg(header, MASK_REGULAR_RUN_LENGTH, src)
            }

            Self::LITE_SET_FG_FGBG_IMAGE => {
                extract_run_length_fg_bg(header, MASK_LITE_RUN_LENGTH, src)
            }

            Self::REGULAR_BG_RUN
            | Self::REGULAR_FG_RUN
            | Self::REGULAR_COLOR_RUN
            | Self::REGULAR_COLOR_IMAGE => extract_run_length_regular(header, src),

            Self::LITE_SET_FG_FG_RUN | Self::LITE_DITHERED_RUN => {
                extract_run_length_lite(header, src)
            }

            Self::MEGA_MEGA_BG_RUN
            | Self::MEGA_MEGA_FG_RUN
            | Self::MEGA_MEGA_SET_FG_RUN
            | Self::MEGA_MEGA_DITHERED_RUN
            | Self::MEGA_MEGA_COLOR_RUN
            | Self::MEGA_MEGA_FGBG_IMAGE
            | Self::MEGA_MEGA_SET_FGBG_IMAGE
            | Self::MEGA_MEGA_COLOR_IMAGE => extract_run_length_mega_mega(src),

            Self::SPECIAL_FGBG_1
            | Self::SPECIAL_FGBG_2
            | Self::SPECIAL_WHITE
            | Self::SPECIAL_BLACK => Ok(0),

            _ => Ok(0),
        }
    }
}

const MASK_REGULAR_RUN_LENGTH: u8 = 0x1F;
const MASK_LITE_RUN_LENGTH: u8 = 0x0F;

/// Extract the run length of a Foreground/Background Image Order.
fn extract_run_length_fg_bg(
    header: u8,
    length_mask: u8,
    src: &mut Buf<'_>,
) -> Result<usize, RleError> {
    match header & length_mask {
        0 => {
            ensure_size!(from: src, size: 1);
            Ok(usize::from(src.read_u8()) + 1)
        }
        run_length => Ok(usize::from(run_length) * 8),
    }
}

/// Extract the run length of a regular-form compression order.
fn extract_run_length_regular(header: u8, src: &mut Buf<'_>) -> Result<usize, RleError> {
    match header & MASK_REGULAR_RUN_LENGTH {
        0 => {
            // An extended (MEGA) run.
            ensure_size!(from: src, size: 1);
            Ok(usize::from(src.read_u8()) + 32)
        }
        run_length => Ok(usize::from(run_length)),
    }
}

fn extract_run_length_lite(header: u8, src: &mut Buf<'_>) -> Result<usize, RleError> {
    match header & MASK_LITE_RUN_LENGTH {
        0 => {
            // An extended (MEGA) run.
            ensure_size!(from: src, size: 1);
            Ok(usize::from(src.read_u8()) + 16)
        }
        run_length => Ok(usize::from(run_length)),
    }
}

fn extract_run_length_mega_mega(src: &mut Buf<'_>) -> Result<usize, RleError> {
    ensure_size!(from: src, size: 2);

    let run_length = usize::from(src.read_u16());

    if run_length == 0 {
        Err(RleError::UnexpectedZeroLength)
    } else {
        Ok(run_length)
    }
}

// TODO: use ironrdp_core::ReadCursor instead
struct Buf<'a> {
    inner: &'a [u8],
    pos: usize,
}

impl<'a> Buf<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            inner: bytes,
            pos: 0,
        }
    }

    fn remaining_len(&self) -> usize {
        self.inner.len() - self.pos
    }

    fn read<const N: usize>(&mut self) -> [u8; N] {
        let bytes = &self.inner[self.pos..self.pos + N];
        self.pos += N;
        bytes.try_into().expect("N-elements array")
    }

    fn read_u8(&mut self) -> u8 {
        u8::from_le_bytes(self.read::<1>())
    }

    fn read_u16(&mut self) -> u16 {
        u16::from_le_bytes(self.read::<2>())
    }

    fn read_u24(&mut self) -> u32 {
        let bytes = self.read::<3>();
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
    }

    fn rewinded(&'a self, len: usize) -> Buf<'a> {
        Buf {
            inner: self.inner,
            pos: self.pos - len,
        }
    }

    fn eof(&self) -> bool {
        self.pos == self.inner.len()
    }
}

// TODO: use ironrdp_core::WriteCursor instead
struct BufMut<'a> {
    inner: &'a mut [u8],
    pos: usize,
}

impl<'a> BufMut<'a> {
    fn new(bytes: &'a mut [u8]) -> Self {
        Self {
            inner: bytes,
            pos: 0,
        }
    }

    fn remaining_len(&self) -> usize {
        self.inner.len() - self.pos
    }

    fn write(&mut self, bytes: &[u8]) {
        self.inner[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
        self.pos += bytes.len();
    }

    fn write_u8(&mut self, value: u8) {
        self.write(&[value]);
    }

    fn write_u16(&mut self, value: u16) {
        self.write(&value.to_le_bytes());
    }

    fn write_u24(&mut self, value: u32) {
        self.write(&value.to_le_bytes()[..3]);
    }

    fn read_pixel_above<Mode: DepthMode>(&self, row_delta: usize) -> Mode::Pixel {
        let read_buf = Buf {
            inner: self.inner,
            pos: self.pos,
        };
        let mut read_buf = read_buf.rewinded(row_delta);
        Mode::read_pixel(&mut read_buf)
    }
}

trait DepthMode {
    type Pixel: Copy + BitXor<Output = Self::Pixel> + Into<u32>;

    /// The color depth (in bytes per pixel) for this mode
    const COLOR_DEPTH: usize;

    /// Bits per pixel
    const BPP: usize;

    /// Pixel format for this depth mode
    const PIXEL_FORMAT: RlePixelFormat;

    /// The black pixel value
    const BLACK_PIXEL: Self::Pixel;

    /// The white pixel value
    const WHITE_PIXEL: Self::Pixel;

    /// Writes a pixel to the specified buffer
    fn write_pixel(dst: &mut BufMut<'_>, pixel: Self::Pixel);

    /// Reads a pixel from the specified buffer
    fn read_pixel(src: &mut Buf<'_>) -> Self::Pixel;
}

struct Mode8Bpp;

impl DepthMode for Mode8Bpp {
    type Pixel = u8;

    const COLOR_DEPTH: usize = 1;

    const BPP: usize = 8;

    const PIXEL_FORMAT: RlePixelFormat = RlePixelFormat::Rgb8;

    const BLACK_PIXEL: Self::Pixel = 0x00;

    const WHITE_PIXEL: Self::Pixel = 0xFF;

    fn write_pixel(dst: &mut BufMut<'_>, pixel: Self::Pixel) {
        dst.write_u8(pixel);
    }

    fn read_pixel(src: &mut Buf<'_>) -> Self::Pixel {
        src.read_u8()
    }
}

struct Mode15Bpp;

impl DepthMode for Mode15Bpp {
    type Pixel = u16;

    const COLOR_DEPTH: usize = 2;

    const BPP: usize = 15;

    const PIXEL_FORMAT: RlePixelFormat = RlePixelFormat::Rgb15;

    const BLACK_PIXEL: Self::Pixel = 0x0000;

    // 5 bits per RGB component:
    // 0111 1111 1111 1111 (binary)
    const WHITE_PIXEL: Self::Pixel = 0x7FFF;

    fn write_pixel(dst: &mut BufMut<'_>, pixel: Self::Pixel) {
        dst.write_u16(pixel);
    }

    fn read_pixel(src: &mut Buf<'_>) -> Self::Pixel {
        src.read_u16()
    }
}

struct Mode16Bpp;

impl DepthMode for Mode16Bpp {
    type Pixel = u16;

    const COLOR_DEPTH: usize = 2;

    const BPP: usize = 16;

    const PIXEL_FORMAT: RlePixelFormat = RlePixelFormat::Rgb16;

    const BLACK_PIXEL: Self::Pixel = 0x0000;

    // 5 bits for red, 6 bits for green, 5 bits for green:
    // 1111 1111 1111 1111 (binary)
    const WHITE_PIXEL: Self::Pixel = 0xFFFF;

    fn write_pixel(dst: &mut BufMut<'_>, pixel: Self::Pixel) {
        dst.write_u16(pixel);
    }

    fn read_pixel(src: &mut Buf<'_>) -> Self::Pixel {
        src.read_u16()
    }
}

struct Mode24Bpp;

impl DepthMode for Mode24Bpp {
    type Pixel = u32;

    const COLOR_DEPTH: usize = 3;

    const BPP: usize = 24;

    const PIXEL_FORMAT: RlePixelFormat = RlePixelFormat::Rgb24;

    const BLACK_PIXEL: Self::Pixel = 0x00_0000;

    // 8 bits per RGB component:
    // 1111 1111 1111 1111 1111 1111 (binary)
    const WHITE_PIXEL: Self::Pixel = 0xFF_FFFF;

    fn write_pixel(dst: &mut BufMut<'_>, pixel: Self::Pixel) {
        dst.write_u24(pixel);
    }

    fn read_pixel(src: &mut Buf<'_>) -> Self::Pixel {
        src.read_u24()
    }
}

/// Writes a foreground/background image to a destination buffer.
fn write_fg_bg_image<Mode: DepthMode>(
    dst: &mut BufMut<'_>,
    row_delta: usize,
    bitmask: u8,
    fg_pel: Mode::Pixel,
    mut c_bits: usize,
) -> Result<(), RleError> {
    ensure_size!(into: dst, size: c_bits * Mode::COLOR_DEPTH);

    let mut mask = 0x01;

    repeat::<8>(|| {
        let above_pixel = dst.read_pixel_above::<Mode>(row_delta);

        if bitmask & mask != 0 {
            Mode::write_pixel(dst, above_pixel ^ fg_pel);
        } else {
            Mode::write_pixel(dst, above_pixel);
        }

        c_bits -= 1;
        mask <<= 1;

        c_bits == 0
    });

    Ok(())
}

/// Writes a foreground/background image to a destination buffer
fn write_first_line_fg_bg_image<Mode: DepthMode>(
    dst: &mut BufMut<'_>,
    bitmask: u8,
    fg_pel: Mode::Pixel,
    mut c_bits: usize,
) -> Result<(), RleError> {
    ensure_size!(into: dst, size: c_bits * Mode::COLOR_DEPTH);

    let mut mask = 0x01;

    repeat::<8>(|| {
        if bitmask & mask != 0 {
            Mode::write_pixel(dst, fg_pel);
        } else {
            Mode::write_pixel(dst, Mode::BLACK_PIXEL);
        }

        c_bits -= 1;
        mask <<= 1;

        c_bits == 0
    });

    Ok(())
}

fn repeat<const N: usize>(mut op: impl FnMut() -> bool) {
    for _ in 0..N {
        let stop = op();

        if stop {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! test_buf_mut {
        ($mode:ident) => {{
            let row_delta = 4 * $mode::COLOR_DEPTH;
            let mut buf = vec![0; row_delta * 2];
            let mut dst = BufMut::new(buf.as_mut_slice());

            $mode::write_pixel(&mut dst, 0xDEAD);
            $mode::write_pixel(&mut dst, 0xBEEF);
            $mode::write_pixel(&mut dst, 0xFADE);
            $mode::write_pixel(&mut dst, 0xFEED);

            assert_eq!(dst.read_pixel_above::<$mode>(row_delta), 0xDEAD);
            $mode::write_pixel(&mut dst, $mode::WHITE_PIXEL);
            assert_eq!(dst.read_pixel_above::<$mode>(row_delta), 0xBEEF);
            $mode::write_pixel(&mut dst, $mode::WHITE_PIXEL);
            assert_eq!(dst.read_pixel_above::<$mode>(row_delta), 0xFADE);
            $mode::write_pixel(&mut dst, $mode::WHITE_PIXEL);
            assert_eq!(dst.read_pixel_above::<$mode>(row_delta), 0xFEED);
            $mode::write_pixel(&mut dst, $mode::WHITE_PIXEL);
        }};
    }

    #[test]
    fn buf_mut_16_bpp() {
        test_buf_mut!(Mode16Bpp);
    }

    #[test]
    fn buf_mut_15_bpp() {
        test_buf_mut!(Mode15Bpp);
    }

    #[test]
    fn buf_mut_24_bpp() {
        test_buf_mut!(Mode24Bpp);
    }
}
