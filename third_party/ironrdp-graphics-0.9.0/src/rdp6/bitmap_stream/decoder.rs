use frd_rdp_legacy_kernels::{KernelError, NativeKernels, PlanarLayout};
use ironrdp_core::{DecodeError, decode};
use ironrdp_pdu::bitmap::rdp6::{BitmapStream as BitmapStreamPdu, ColorPlaneDefinition};

use crate::color_conversion::Rgb;
use crate::rdp6::rle::{NativePlanes, PlaneMaterializer, RleDecodeError, ScalarPlanes};

#[derive(Debug)]
pub enum BitmapDecodeError {
    Decode(DecodeError),
    Rle(RleDecodeError),
    InvalidUncompressedDataSize,
}

impl core::fmt::Display for BitmapDecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BitmapDecodeError::Decode(_error) => {
                write!(f, "failed to decode RDP6 bitmap stream PDU")
            }
            BitmapDecodeError::Rle(_error) => {
                write!(f, "failed to perform RLE decompression of RDP6 bitmap stream")
            }
            BitmapDecodeError::InvalidUncompressedDataSize => write!(
                f,
                "color plane data size provided in PDU is not sufficient to reconstruct the bitmap"
            ),
        }
    }
}

impl core::error::Error for BitmapDecodeError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            BitmapDecodeError::Decode(err) => Some(err),
            BitmapDecodeError::Rle(err) => Some(err),
            BitmapDecodeError::InvalidUncompressedDataSize => None,
        }
    }
}

impl From<DecodeError> for BitmapDecodeError {
    fn from(err: DecodeError) -> Self {
        BitmapDecodeError::Decode(err)
    }
}

impl From<RleDecodeError> for BitmapDecodeError {
    fn from(err: RleDecodeError) -> Self {
        BitmapDecodeError::Rle(err)
    }
}

/// checked 原生入口的错误保留 parser 或 kernel 原始来源。
#[derive(Debug)]
pub enum NativeBitmapError {
    Bitmap(BitmapDecodeError),
    Kernel(KernelError),
}
impl core::fmt::Display for NativeBitmapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Bitmap(error) => error.fmt(f),
            Self::Kernel(error) => error.fmt(f),
        }
    }
}
impl core::error::Error for NativeBitmapError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(match self {
            Self::Bitmap(error) => error,
            Self::Kernel(error) => error,
        })
    }
}
impl From<BitmapDecodeError> for NativeBitmapError {
    fn from(error: BitmapDecodeError) -> Self {
        Self::Bitmap(error)
    }
}
impl From<RleDecodeError> for NativeBitmapError {
    fn from(error: RleDecodeError) -> Self {
        Self::Bitmap(error.into())
    }
}
impl From<KernelError> for NativeBitmapError {
    fn from(error: KernelError) -> Self {
        Self::Kernel(error)
    }
}

/// Implements decoding of RDP6 bitmap stream PDU (see [`BitmapStreamPdu`])
#[derive(Debug, Default)]
pub struct BitmapStreamDecoder {
    /// Optimization to avoid reallocations, re-use this buffer for all bitmaps in the session
    planes_buffer: Vec<u8>,
}

/// Internal implementation of RDP6 bitmap stream PDU decoder for specific image size and format
struct BitmapStreamDecoderImpl<'a> {
    bitmap: BitmapStreamPdu<'a>,
    image_width: usize,
    image_height: usize,
    chroma_width: usize,
    chroma_height: usize,
    full_plane_size: usize,
    chroma_plane_size: usize,
    uncompressed_planes_size: usize,
    color_plane_offsets: [usize; 3],
}

struct AYCoCgParams {
    color_loss_level: u8,
    chroma_subsampling: bool,
    alpha: bool,
}

impl<'a> BitmapStreamDecoderImpl<'a> {
    fn init(bitmap: BitmapStreamPdu<'a>, image_width: usize, image_height: usize) -> Self {
        let (chroma_width, chroma_height) = if bitmap.has_subsampled_chroma() {
            // When image is subsampled, chroma plane has half the size of the luma plane, however
            // its size is rounded up to the nearest greater integer, to take into account odd image
            // size (e.g. if width is 3, then chroma plane width is 2, not 1, to take into account
            // the odd column which expands to 1 pixel instead of 2 during supersampling)
            (image_width.div_ceil(2), image_height.div_ceil(2))
        } else {
            (image_width, image_height)
        };

        let full_plane_size = image_width * image_height;
        let chroma_plane_size = chroma_width * chroma_height;

        let uncompressed_planes_size = if bitmap.has_subsampled_chroma() {
            full_plane_size + chroma_plane_size * 2
        } else {
            full_plane_size * 3
        };

        let color_plane_offsets = [0, full_plane_size, full_plane_size + chroma_plane_size];

        Self {
            bitmap,
            image_width,
            image_height,
            chroma_width,
            chroma_height,
            full_plane_size,
            chroma_plane_size,
            uncompressed_planes_size,
            color_plane_offsets,
        }
    }

    fn decompress_planes<M, E>(&'a self, aux_buffer: &'a mut Vec<u8>, materializer: &mut M) -> Result<&'a [u8], E>
    where
        M: PlaneMaterializer,
        E: From<M::Error> + From<BitmapDecodeError>,
    {
        let planes = if self.bitmap.header.enable_rle_compression {
            // We don't care for the previous content, just resize it to fit the data
            aux_buffer.resize(self.uncompressed_planes_size, 0);
            let uncompressed_planes_buffer = &mut aux_buffer[..self.uncompressed_planes_size];

            let compressed = self.bitmap.color_panes_data();
            let mut src_offset = 0;

            // Decompress Alpha plane
            if self.bitmap.header.use_alpha {
                // alpha 仍须完整解码与消费；其值不要求为 0xFF。
                src_offset += materializer
                    .decompress(
                        &compressed[src_offset..],
                        uncompressed_planes_buffer,
                        self.image_width,
                        self.image_height,
                    )
                    .map_err(E::from)?;
            }

            // Decompress R/Y plane
            src_offset += materializer
                .decompress(
                    &compressed[src_offset..],
                    &mut uncompressed_planes_buffer[self.color_plane_offsets[0]..],
                    self.image_width,
                    self.image_height,
                )
                .map_err(E::from)?;

            // Decompress G/Co plane
            src_offset += materializer
                .decompress(
                    &compressed[src_offset..],
                    &mut uncompressed_planes_buffer[self.color_plane_offsets[1]..],
                    self.chroma_width,
                    self.chroma_height,
                )
                .map_err(E::from)?;

            // Decompress B/Cg plane
            materializer
                .decompress(
                    &compressed[src_offset..],
                    &mut uncompressed_planes_buffer[self.color_plane_offsets[2]..],
                    self.chroma_width,
                    self.chroma_height,
                )
                .map_err(E::from)?;

            &uncompressed_planes_buffer[..self.uncompressed_planes_size]
        } else {
            // Discard alpha plane
            let color_planes_offset = if self.bitmap.header.use_alpha {
                self.full_plane_size
            } else {
                0
            };

            let expected_data_size = color_planes_offset + self.uncompressed_planes_size;

            if self.bitmap.color_panes_data().len() < expected_data_size {
                return Err(BitmapDecodeError::InvalidUncompressedDataSize.into());
            }

            &self.bitmap.color_panes_data()[color_planes_offset..]
        };

        Ok(planes)
    }

    fn write_argb_planes_to_rgb24(&self, planes: &[u8], dst: &mut Vec<u8>) {
        // For ARGB conversion is simple - just copy data in correct order
        let (r_offset, g_offset, b_offset) = (
            self.color_plane_offsets[0],
            self.color_plane_offsets[1],
            self.color_plane_offsets[2],
        );

        let r_plane = &planes[r_offset..r_offset + self.full_plane_size];
        let g_plane = &planes[g_offset..g_offset + self.full_plane_size];
        let b_plane = &planes[b_offset..b_offset + self.full_plane_size];

        for i in 0..self.full_plane_size {
            let (r, g, b) = (r_plane[i], g_plane[i], b_plane[i]);

            dst.extend_from_slice(&[r, g, b]);
        }
    }

    fn write_aycocg_planes_to_rgb24(&self, params: AYCoCgParams, planes: &[u8], dst: &mut Vec<u8>) {
        #![allow(clippy::similar_names, reason = "it’s hard to find better names for co, cg, etc")]

        let sample_shift = usize::from(params.chroma_subsampling);

        let (y_offset, co_offset, cg_offset) = (
            self.color_plane_offsets[0],
            self.color_plane_offsets[1],
            self.color_plane_offsets[2],
        );

        let y_plane = &planes[y_offset..y_offset + self.full_plane_size];
        let co_plane = &planes[co_offset..co_offset + self.chroma_plane_size];
        let cg_plane = &planes[cg_offset..cg_offset + self.chroma_plane_size];

        for (idx, y) in y_plane.iter().copied().enumerate() {
            let chroma_row = (idx / self.image_width) >> sample_shift;
            let chroma_col = (idx % self.image_width) >> sample_shift;
            let chroma_idx = chroma_row * self.chroma_width + chroma_col;

            let co = co_plane[chroma_idx];
            let cg = cg_plane[chroma_idx];

            let Rgb { r, g, b } = ycocg_with_cll_to_rgb(params.color_loss_level, y, co, cg);

            // As described in 3.1.9.1.2 [MS-RDPEGDI], R and B channels are swapped for
            // AYCoCg when 24-bit image is used (no alpha). We swap them back here
            if params.alpha {
                dst.extend_from_slice(&[r, g, b]);
            } else {
                dst.extend_from_slice(&[b, g, r]);
            }
        }
    }

    fn init_checked(bitmap: BitmapStreamPdu<'a>, width: usize, height: usize) -> Result<Self, KernelError> {
        checked_output_size(width, height)?;
        let full = width.checked_mul(height).ok_or(KernelError::InvalidDimensions)?;
        let chroma = if bitmap.has_subsampled_chroma() {
            width.div_ceil(2).checked_mul(height.div_ceil(2))
        } else {
            Some(full)
        }
        .ok_or(KernelError::InvalidDimensions)?;
        let planes = chroma
            .checked_mul(2)
            .and_then(|n| n.checked_add(full))
            .ok_or(KernelError::InvalidDimensions)?;
        let input_size = planes
            .checked_add(if bitmap.header.use_alpha { full } else { 0 })
            .ok_or(KernelError::InvalidDimensions)?;
        if input_size > isize::MAX as usize {
            return Err(KernelError::InvalidDimensions);
        }
        Ok(Self::init(bitmap, width, height))
    }

    fn decode_native(
        self,
        dst: &mut Vec<u8>,
        aux_buffer: &'a mut Vec<u8>,
        native: &mut NativePlanes<'_>,
    ) -> Result<(), NativeBitmapError> {
        let append = checked_output_size(self.image_width, self.image_height)?;
        let start = dst.len();
        let end = start
            .checked_add(append)
            .filter(|&n| n <= isize::MAX as usize)
            .ok_or(KernelError::InvalidDimensions)?;
        dst.try_reserve(append).map_err(|_| KernelError::InvalidDimensions)?;
        if self.bitmap.header.enable_rle_compression {
            aux_buffer
                .try_reserve(self.uncompressed_planes_size.saturating_sub(aux_buffer.len()))
                .map_err(|_| KernelError::InvalidDimensions)?;
        }
        let planes = self.decompress_planes::<_, NativeBitmapError>(aux_buffer, native)?;
        let first = &planes[..self.full_plane_size];
        let second = &planes[self.color_plane_offsets[1]..self.color_plane_offsets[1] + self.chroma_plane_size];
        let third = &planes[self.color_plane_offsets[2]..self.color_plane_offsets[2] + self.chroma_plane_size];
        // 全部平面成功后才追加空间；零初始化是 Vec 新内存初始化，不是解码 materialization。
        dst.resize(end, 0);
        let result = match self.bitmap.header.color_plane_definition {
            ColorPlaneDefinition::Argb => {
                #[cfg(test)]
                {
                    native.calls.rgb += 1;
                }
                native
                    .kernels
                    .rgb_planes_to_rgb24(first, second, third, &mut dst[start..])
            }
            ColorPlaneDefinition::AYCoCg {
                color_loss_level,
                use_chroma_subsampling,
                ..
            } => {
                #[cfg(test)]
                {
                    native.calls.ycocg += 1;
                }
                let layout = PlanarLayout {
                    width: self.image_width,
                    height: self.image_height,
                    color_loss_level,
                    subsampled: use_chroma_subsampling,
                    has_alpha: self.bitmap.header.use_alpha,
                };
                native
                    .kernels
                    .ycocg_planes_to_rgb24(layout, first, second, third, &mut dst[start..])
            }
        };
        if let Err(error) = result {
            dst.truncate(start);
            return Err(error.into());
        }
        Ok(())
    }

    fn decode(self, dst: &mut Vec<u8>, aux_buffer: &'a mut Vec<u8>) -> Result<(), BitmapDecodeError> {
        // Reserve enough space for decoded RGB channels data
        dst.reserve(self.image_height * self.image_width * 3);

        match self.bitmap.header.color_plane_definition {
            ColorPlaneDefinition::Argb => {
                let color_planes = self.decompress_planes::<_, BitmapDecodeError>(aux_buffer, &mut ScalarPlanes)?;
                self.write_argb_planes_to_rgb24(color_planes, dst);
            }
            ColorPlaneDefinition::AYCoCg {
                color_loss_level,
                use_chroma_subsampling,
                ..
            } => {
                let params: AYCoCgParams = AYCoCgParams {
                    color_loss_level,
                    chroma_subsampling: use_chroma_subsampling,
                    alpha: self.bitmap.header.use_alpha,
                };
                let color_planes = self.decompress_planes::<_, BitmapDecodeError>(aux_buffer, &mut ScalarPlanes)?;
                self.write_aycocg_planes_to_rgb24(params, color_planes, dst);
            }
        }

        Ok(())
    }
}

/// Perform YCoCg -> RGB conversion with color loss reduction (CLL) correction.
fn ycocg_with_cll_to_rgb(cll: u8, y: u8, co: u8, cg: u8) -> Rgb {
    #![allow(clippy::similar_names)] // It’s hard to find better names for co, cg, etc.

    // We decrease CLL by 1 to skip division by 2 for co & cg components during computation of
    // the following color conversion matrix:
    // |R|   |1   1/2   -1/2|   |Y |
    // |G| = |1    0     1/2| * |Co|
    // |B|   |1  -1/2   -1/2|   |Cg|
    let chroma_shift = cll - 1;

    let clip_i16 =
        |v: i16| u8::try_from(v.clamp(0, 255)).expect("fits into u8 because the value is clamped to [0..256]");

    let co_signed = (co << chroma_shift).cast_signed();
    let cg_signed = (cg << chroma_shift).cast_signed();

    let y = i16::from(y);
    let co = i16::from(co_signed);
    let cg = i16::from(cg_signed);

    let t = y - cg;
    let r = clip_i16(t + co);
    let g = clip_i16(y + cg);
    let b = clip_i16(t - co);

    Rgb { r, g, b }
}

fn checked_output_size(width: usize, height: usize) -> Result<usize, KernelError> {
    if width == 0 || height == 0 {
        return Err(KernelError::InvalidDimensions);
    }
    width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(3))
        .filter(|&n| n <= isize::MAX as usize)
        .ok_or(KernelError::InvalidDimensions)
}

impl BitmapStreamDecoder {
    /// 显式 checked 原生入口；不改变旧入口或 session 默认选择。
    /// RGB/append 几何在 PDU 前检查，header-specific 平面总长在 PDU 后、写入前检查。
    /// 合法几何保留原 parser 的错误顺序。
    pub fn decode_bitmap_stream_to_rgb24_native(
        &mut self,
        bitmap_data: &[u8],
        dst: &mut Vec<u8>,
        width: usize,
        height: usize,
        kernels: &NativeKernels,
    ) -> Result<(), NativeBitmapError> {
        let append = checked_output_size(width, height)?;
        dst.len()
            .checked_add(append)
            .filter(|&n| n <= isize::MAX as usize)
            .ok_or(KernelError::InvalidDimensions)?;
        let bitmap = decode::<BitmapStreamPdu<'_>>(bitmap_data).map_err(BitmapDecodeError::from)?;
        let decoder = BitmapStreamDecoderImpl::init_checked(bitmap, width, height)?;
        decoder.decode_native(dst, &mut self.planes_buffer, &mut NativePlanes::new(kernels))
    }

    /// Performs decoding of bitmap stream PDU from `bitmap_data` and writes decoded rgb24
    /// image to `dst` buffer.
    pub fn decode_bitmap_stream_to_rgb24(
        &mut self,
        bitmap_data: &[u8],
        dst: &mut Vec<u8>,
        image_width: usize,
        image_height: usize,
    ) -> Result<(), BitmapDecodeError> {
        let bitmap = decode::<BitmapStreamPdu<'_>>(bitmap_data)?;

        let decoder = BitmapStreamDecoderImpl::init(bitmap, image_width, image_height);

        decoder.decode(dst, &mut self.planes_buffer)
    }
}
