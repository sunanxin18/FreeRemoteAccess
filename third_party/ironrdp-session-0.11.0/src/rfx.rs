use core::cmp::min;

use ironrdp_graphics::color_conversion::{self, YCbCrBuffer};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_graphics::legacy_native::{self, NativeKernels};
use ironrdp_graphics::rectangle_processing::Region;
use ironrdp_graphics::{dwt, quantization, rlgr, subband_reconstruction};
use ironrdp_pdu::codecs::rfx::{self, EntropyAlgorithm, Quant, RfxRectangle, Tile};
use ironrdp_pdu::geometry::{InclusiveRectangle, Rectangle as _};
use ironrdp_pdu::{Decode as _, ReadCursor, decode_cursor};
use tracing::{instrument, trace};

use crate::image::{CoverageCollector, DecodedImage};
use crate::{SessionResult, custom_err, general_err, reason_err};

const TILE_SIZE: u16 = 64;

pub type FrameId = u32;

pub struct DecodingContext {
    context: rfx::ContextPdu,
    channels: rfx::ChannelsPdu,
    decoding_tiles: DecodingTileContext,
    native_legacy_kernels: Option<NativeKernels>,
}

impl Default for DecodingContext {
    fn default() -> Self {
        Self {
            context: rfx::ContextPdu {
                flags: rfx::OperatingMode::empty(),
                entropy_algorithm: EntropyAlgorithm::Rlgr1,
            },
            channels: rfx::ChannelsPdu(Vec::new()),
            decoding_tiles: DecodingTileContext::new(),
            native_legacy_kernels: None,
        }
    }
}

impl DecodingContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// 显式选择已验证 CPU 的原生 legacy 能力；默认仍使用原版 decoder。
    pub fn set_native_legacy_kernels(&mut self, kernels: NativeKernels) {
        self.native_legacy_kernels = Some(kernels);
    }

    pub fn decode(
        &mut self,
        image: &mut DecodedImage,
        destination: &InclusiveRectangle,
        input: &mut ReadCursor<'_>,
    ) -> SessionResult<(FrameId, InclusiveRectangle)> {
        let (frame, region) = self.decode_region(image, destination, input)?;
        Ok((frame, region.unwrap_or_else(InclusiveRectangle::empty)))
    }

    /// Native no-op 保留 None；既有公开 decode 的默认返回约定保持。
    pub(crate) fn decode_region(
        &mut self,
        image: &mut DecodedImage,
        destination: &InclusiveRectangle,
        input: &mut ReadCursor<'_>,
    ) -> SessionResult<(FrameId, Option<InclusiveRectangle>)> {
        self.decode_region_collected(image, destination, input, &mut CoverageCollector::default())
    }

    pub(crate) fn decode_region_collected(
        &mut self,
        image: &mut DecodedImage,
        destination: &InclusiveRectangle,
        input: &mut ReadCursor<'_>,
        coverage: &mut CoverageCollector,
    ) -> SessionResult<(FrameId, Option<InclusiveRectangle>)> {
        loop {
            let block = rfx::Block::decode(input).map_err(|e| custom_err!("decode block", e))?;

            match block {
                rfx::Block::Sync(_) => {
                    self.process_sync(input)?;
                }
                rfx::Block::CodecChannel(rfx::CodecChannel::FrameBegin(f)) => {
                    return self.process_frame(f, input, image, destination, coverage);
                }
                _ => {
                    return Err(reason_err!(
                        "rfx::DecodingContext",
                        "unexpected RFX block type: {:?}",
                        block.block_type()
                    ));
                }
            }
        }
    }

    fn process_sync(&mut self, input: &mut ReadCursor<'_>) -> SessionResult<()> {
        self.process_headers(input)
    }

    fn process_headers(&mut self, input: &mut ReadCursor<'_>) -> SessionResult<()> {
        let mut context = None;
        let mut channels = None;

        // headers can appear in any order: CodecVersions, Channels, Context
        for _ in 0..3 {
            match decode_cursor(input).map_err(|e| custom_err!("decode headers", e))? {
                rfx::Block::CodecChannel(rfx::CodecChannel::Context(c)) => context = Some(c),
                rfx::Block::Channels(c) => channels = Some(c),
                rfx::Block::CodecVersions(_) => (),
                _ => {
                    return Err(general_err!("unexpected RFX block type"));
                }
            }
        }

        let context = context.ok_or_else(|| general_err!("context header is missing"))?;
        let channels = channels.ok_or_else(|| general_err!("channels header is missing"))?;

        if channels.0.is_empty() {
            return Err(general_err!("no RFX channel announced"));
        }

        self.context = context;
        self.channels = channels;

        Ok(())
    }

    #[instrument(skip_all)]
    fn process_frame(
        &mut self,
        frame_begin: rfx::FrameBeginPdu,
        input: &mut ReadCursor<'_>,
        image: &mut DecodedImage,
        destination: &InclusiveRectangle,
        coverage: &mut CoverageCollector,
    ) -> SessionResult<(FrameId, Option<InclusiveRectangle>)> {
        let channel = self
            .channels
            .0
            .first()
            .ok_or_else(|| general_err!("no RFX channel found"))?;
        let width = channel.width.try_into().map_err(|_| general_err!("invalid width"))?;
        let height = channel.height.try_into().map_err(|_| general_err!("invalid height"))?;
        let entropy_algorithm = self.context.entropy_algorithm;

        let region: rfx::Block<'_> = decode_cursor(input).map_err(|e| custom_err!("decode region", e))?;
        let mut region = match region {
            rfx::Block::CodecChannel(rfx::CodecChannel::Region(region)) => region,
            _ => return Err(general_err!("unexpected block type")),
        };
        let tile_set: rfx::Block<'_> = decode_cursor(input).map_err(|e| custom_err!("decode tile_set", e))?;
        let tile_set = match tile_set {
            rfx::Block::CodecChannel(rfx::CodecChannel::TileSet(t)) => t,
            _ => return Err(general_err!("unexpected block type")),
        };
        let frame_end: rfx::Block<'_> = decode_cursor(input).map_err(|e| custom_err!("decode frame_end", e))?;
        if !matches!(frame_end, rfx::Block::CodecChannel(rfx::CodecChannel::FrameEnd(_))) {
            return Err(general_err!("unexpected block type"));
        }

        if region.rectangles.is_empty() {
            region.rectangles = vec![RfxRectangle {
                x: 0,
                y: 0,
                width,
                height,
            }];
        }
        let region = region;

        trace!(frame_index = frame_begin.index);
        trace!(destination_rectangle = ?destination);
        trace!(context = ?self.context);
        trace!(channels = ?self.channels);
        trace!(?region);

        let clipping_rectangles = if coverage.enabled {
            checked_clipping_rectangles(region.rectangles.as_slice(), destination, width, height, true)?
        } else if self.native_legacy_kernels.is_some() {
            native_clipping_rectangles(region.rectangles.as_slice(), destination, width, height)?
        } else {
            clipping_rectangles(region.rectangles.as_slice(), destination, width, height)
        };
        trace!("Clipping rectangles: {:?}", clipping_rectangles);
        let mut final_update_rectangle = if self.native_legacy_kernels.is_some() {
            None
        } else {
            Some(clipping_rectangles.extents.clone())
        };
        // Native 几何与 quant 索引先于 tile publication 验证；默认保留原惰性迭代次序。
        if self.native_legacy_kernels.is_some() || coverage.enabled {
            for tile in &tile_set.tiles {
                native_tile_rectangle(tile, destination)?;
                for index in [tile.y_quant_index, tile.cb_quant_index, tile.cr_quant_index] {
                    if tile_set.quants.get(usize::from(index)).is_none() {
                        return Err(custom_err!(
                            "native RFX quant index",
                            legacy_native::KernelError::Length
                        ));
                    }
                }
            }
        }
        // Native 已预先证明原 tile 算术不溢出；原 iterator 不新增 allocation/default 错误优先序。
        for (update_rectangle, tile_data) in tiles_to_rectangles(tile_set.tiles.as_slice(), destination)
            .zip(map_tiles_data(tile_set.tiles.as_slice(), tile_set.quants.as_slice()))
        {
            decode_tile(
                &tile_data,
                entropy_algorithm,
                self.decoding_tiles.tile_output.as_mut(),
                self.decoding_tiles.ycbcr_buffer.as_mut(),
                self.decoding_tiles.ycbcr_temp_buffer.as_mut(),
                self.native_legacy_kernels.as_ref(),
            )?;

            let target_fits = image.width() > clipping_rectangles.extents.right
                && image.height() > clipping_rectangles.extents.bottom;
            let actual = || {
                clipping_rectangles.rectangles.iter().filter_map(|r| {
                    if !target_fits {
                        return None;
                    }
                    let left = r.left.max(update_rectangle.left);
                    let top = r.top.max(update_rectangle.top);
                    let right = r.right.min(update_rectangle.right);
                    let bottom = r.bottom.min(update_rectangle.bottom);
                    (left <= right && top <= bottom).then_some(InclusiveRectangle {
                        left,
                        top,
                        right,
                        bottom,
                    })
                })
            };
            let mut exact_regions = Region::new();
            if coverage.enabled {
                let count = actual().count();
                coverage.reserve_rectangles(count)?;
                exact_regions
                    .rectangles
                    .try_reserve_exact(count)
                    .map_err(|e| custom_err!("RFX exact allocation", e))?;
                exact_regions.rectangles.extend(actual());
                exact_regions.extents = InclusiveRectangle::union_all(&exact_regions.rectangles);
            }
            let current_update_rectangle = if coverage.enabled {
                image.apply_tile_exact(
                    &self.decoding_tiles.tile_output,
                    PixelFormat::RgbA32,
                    &exact_regions,
                    &update_rectangle,
                    self.native_legacy_kernels.as_ref(),
                )?
            } else if let Some(kernels) = self.native_legacy_kernels.as_ref() {
                image.apply_tile_native(
                    &self.decoding_tiles.tile_output,
                    PixelFormat::RgbA32,
                    &clipping_rectangles,
                    &update_rectangle,
                    kernels,
                )?
            } else {
                Some(image.apply_tile(
                    &self.decoding_tiles.tile_output,
                    PixelFormat::RgbA32,
                    &clipping_rectangles,
                    &update_rectangle,
                )?)
            };
            if let Some(current) = current_update_rectangle {
                coverage.content(exact_regions.rectangles);
                final_update_rectangle =
                    Some(final_update_rectangle.map_or_else(|| current.clone(), |prior| prior.union(&current)));
            }
        }

        Ok((frame_begin.index, final_update_rectangle))
    }
}

#[derive(Debug, Clone)]
struct DecodingTileContext {
    tile_output: Vec<u8>,
    ycbcr_buffer: Vec<Vec<i16>>,
    ycbcr_temp_buffer: Vec<i16>,
}

impl DecodingTileContext {
    fn new() -> Self {
        let tile_size = usize::from(TILE_SIZE);
        Self {
            tile_output: vec![0; tile_size * tile_size * 4],
            ycbcr_buffer: vec![vec![0; tile_size * tile_size]; 3],
            ycbcr_temp_buffer: vec![0; tile_size * tile_size],
        }
    }
}

fn decode_tile(
    tile: &TileData<'_>,
    entropy_algorithm: EntropyAlgorithm,
    output: &mut [u8],
    ycbcr_temp: &mut [Vec<i16>],
    temp: &mut [i16],
    kernels: Option<&NativeKernels>,
) -> SessionResult<()> {
    for ((quant, data), ycbcr_buffer) in tile.quants.iter().zip(tile.data.iter()).zip(ycbcr_temp.iter_mut()) {
        if let Some(kernels) = kernels {
            legacy_native::decode_component_native(
                quant,
                entropy_algorithm,
                data,
                ycbcr_buffer.as_mut_slice(),
                temp,
                kernels,
            )
            .map_err(|e| custom_err!("decode_component", e))?;
        } else {
            decode_component(quant, entropy_algorithm, data, ycbcr_buffer.as_mut_slice(), temp)?;
        }
    }

    let ycbcr_buffer = YCbCrBuffer {
        y: ycbcr_temp[0].as_slice(),
        cb: ycbcr_temp[1].as_slice(),
        cr: ycbcr_temp[2].as_slice(),
    };

    if let Some(kernels) = kernels {
        kernels
            .ycbcr_to_rgba(ycbcr_buffer.y, ycbcr_buffer.cb, ycbcr_buffer.cr, output)
            .map_err(|e| custom_err!("decode_tile", e))?;
    } else {
        color_conversion::ycbcr_to_rgba(ycbcr_buffer, output).map_err(|e| custom_err!("decode_tile", e))?;
    }

    Ok(())
}

fn decode_component(
    quant: &Quant,
    entropy_algorithm: EntropyAlgorithm,
    data: &[u8],
    output: &mut [i16],
    temp: &mut [i16],
) -> SessionResult<()> {
    rlgr::decode(entropy_algorithm, data, output).map_err(|e| custom_err!("decode_component", e))?;
    subband_reconstruction::decode(&mut output[4032..]);
    quantization::decode(output, quant);
    dwt::decode(output, temp);

    Ok(())
}

fn native_clipping_rectangles(
    rectangles: &[RfxRectangle],
    destination: &InclusiveRectangle,
    width: u16,
    height: u16,
) -> SessionResult<Region> {
    checked_clipping_rectangles(rectangles, destination, width, height, false)
}
fn checked_clipping_rectangles(
    rectangles: &[RfxRectangle],
    destination: &InclusiveRectangle,
    width: u16,
    height: u16,
    exact: bool,
) -> SessionResult<Region> {
    if width == 0 || height == 0 {
        return Err(custom_err!(
            "native RFX geometry",
            legacy_native::KernelError::InvalidDimensions
        ));
    }
    let mut region = Region::new();
    if exact {
        region
            .rectangles
            .try_reserve_exact(rectangles.len())
            .map_err(|e| custom_err!("RFX clip allocation", e))?;
    }
    for rect in rectangles {
        if rect.width == 0 || rect.height == 0 {
            return Err(custom_err!(
                "native RFX geometry",
                legacy_native::KernelError::InvalidDimensions
            ));
        }
        let left = u32::from(destination.left) + u32::from(rect.x);
        let top = u32::from(destination.top) + u32::from(rect.y);
        let right = left + u32::from(rect.width) - 1;
        let bottom = top + u32::from(rect.height) - 1;
        // 不把空交集夹成画面边缘的一个像素。
        if left >= u32::from(width) || top >= u32::from(height) {
            continue;
        }
        let rectangle = InclusiveRectangle {
            left: u16::try_from(left).map_err(|e| custom_err!("native RFX geometry", e))?,
            top: u16::try_from(top).map_err(|e| custom_err!("native RFX geometry", e))?,
            right: u16::try_from(right.min(u32::from(width) - 1)).map_err(|e| custom_err!("native RFX geometry", e))?,
            bottom: u16::try_from(bottom.min(u32::from(height) - 1))
                .map_err(|e| custom_err!("native RFX geometry", e))?,
        };
        if exact {
            // 上游 union 的半开 band 比较可丢单行。rich 保留精确 checked 列表；允许重叠。
            region.extents = if region.rectangles.is_empty() {
                rectangle.clone()
            } else {
                region.extents.union(&rectangle)
            };
            region.rectangles.push(rectangle);
        } else {
            region.union_rectangle(rectangle);
        }
    }
    if exact {
        region
            .rectangles
            .sort_unstable_by_key(|r| (r.top, r.bottom, r.left, r.right));
    }
    Ok(region)
}
fn native_tile_rectangle(tile: &Tile<'_>, destination: &InclusiveRectangle) -> SessionResult<InclusiveRectangle> {
    let left = tile
        .x
        .checked_mul(TILE_SIZE)
        .and_then(|n| destination.left.checked_add(n))
        .ok_or_else(|| {
            custom_err!(
                "native RFX tile geometry",
                legacy_native::KernelError::InvalidDimensions
            )
        })?;
    let top = tile
        .y
        .checked_mul(TILE_SIZE)
        .and_then(|n| destination.top.checked_add(n))
        .ok_or_else(|| {
            custom_err!(
                "native RFX tile geometry",
                legacy_native::KernelError::InvalidDimensions
            )
        })?;
    Ok(InclusiveRectangle {
        left,
        top,
        right: left.checked_add(TILE_SIZE - 1).ok_or_else(|| {
            custom_err!(
                "native RFX tile geometry",
                legacy_native::KernelError::InvalidDimensions
            )
        })?,
        bottom: top.checked_add(TILE_SIZE - 1).ok_or_else(|| {
            custom_err!(
                "native RFX tile geometry",
                legacy_native::KernelError::InvalidDimensions
            )
        })?,
    })
}

fn clipping_rectangles(
    rectangles: &[RfxRectangle],
    destination: &InclusiveRectangle,
    width: u16,
    height: u16,
) -> Region {
    let mut clipping_rectangles = Region::new();

    rectangles
        .iter()
        .map(|r| InclusiveRectangle {
            left: min(destination.left + r.x, width - 1),
            top: min(destination.top + r.y, height - 1),
            right: min(destination.left + r.x + r.width - 1, width - 1),
            bottom: min(destination.top + r.y + r.height - 1, height - 1),
        })
        .for_each(|r| clipping_rectangles.union_rectangle(r));

    clipping_rectangles
}

fn tiles_to_rectangles<'a>(
    tiles: &'a [Tile<'_>],
    destination: &'a InclusiveRectangle,
) -> impl Iterator<Item = InclusiveRectangle> + 'a {
    tiles.iter().map(|t| InclusiveRectangle {
        left: destination.left + t.x * TILE_SIZE,
        top: destination.top + t.y * TILE_SIZE,
        right: destination.left + t.x * TILE_SIZE + TILE_SIZE - 1,
        bottom: destination.top + t.y * TILE_SIZE + TILE_SIZE - 1,
    })
}

fn map_tiles_data<'a>(tiles: &[Tile<'a>], quants: &[Quant]) -> Vec<TileData<'a>> {
    tiles
        .iter()
        .map(|t| TileData {
            quants: [
                quants[usize::from(t.y_quant_index)].clone(),
                quants[usize::from(t.cb_quant_index)].clone(),
                quants[usize::from(t.cr_quant_index)].clone(),
            ],
            data: [t.y_data, t.cb_data, t.cr_data],
        })
        .collect()
}

struct TileData<'a> {
    quants: [Quant; 3],
    data: [&'a [u8]; 3],
}
