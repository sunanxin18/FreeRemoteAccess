//! Apple MVS type-0 rectangle decoder for verified tile modes 0 through 5.

use anyhow::{bail, Context, Result};
use std::sync::Arc;

use crate::vnc::mvs::MAX_MVS_DECODE_PIXELS;
use crate::vnc::mvs_bitstream::{decode_repeat_count, BitReader};
use crate::vnc::mvs_wire::{MvsFullRecord, MvsTables};

const TILE_EDGE: usize = 8;
const RGB_CHANNELS: usize = 3;
const APPLE_FRAMEBUFFER_PIXEL_BYTES: usize = 4;
const STREAM_TERMINAL: u8 = 0x6d;
const APPLE_NATURAL_ORDER: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];
const DC_MAX_UNARY_ONES: usize = 38;
const FIRST_AC_MAX_UNARY_ONES: usize = 32;
const SECOND_AC_MAX_UNARY_ONES: usize = 18;
const MVS_CACHE_SLOT_COUNT: usize = 65_000;
const MVS_CACHE_ENTRY_BYTES: usize = 99;
const MVS_CACHE_LAST_USABLE_INDEX: u16 = 64_999;

const APPLE_LUMA_AC_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125];
const APPLE_LUMA_AC_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];
const APPLE_CHROMA_AC_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119];
const APPLE_CHROMA_AC_VALUES: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

#[derive(Clone, Copy)]
struct AppleHuffmanTable {
    bits: &'static [u8; 16],
    values: &'static [u8; 162],
}

const APPLE_LUMA_AC_HUFFMAN: AppleHuffmanTable = AppleHuffmanTable {
    bits: &APPLE_LUMA_AC_BITS,
    values: &APPLE_LUMA_AC_VALUES,
};
const APPLE_CHROMA_AC_HUFFMAN: AppleHuffmanTable = AppleHuffmanTable {
    bits: &APPLE_CHROMA_AC_BITS,
    values: &APPLE_CHROMA_AC_VALUES,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedMvsRect {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct MvsFullDecoder {
    // 量化表属于 generation-scoped decoder；prepare 仅读取，commit 只安装
    // 完整准备好的下一状态。
    tables: MvsTables,
    committed_records: u64,
    surface_state: Option<MvsSurfaceState>,
    cache_state: MvsCacheState,
}

#[derive(Clone, Debug)]
struct MvsCacheState {
    entries: Arc<Vec<Option<Arc<[i8; MVS_CACHE_ENTRY_BYTES]>>>>,
    previous_cache_index: u16,
    last_insert_index: u16,
    population_count: u32,
}

impl MvsCacheState {
    fn new() -> Self {
        Self {
            entries: Arc::new(vec![None; MVS_CACHE_SLOT_COUNT]),
            previous_cache_index: 0,
            last_insert_index: 0,
            population_count: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SavedCoefficientSeed {
    selected_threshold: u8,
    y: [i8; 64],
    cb_count: u8,
    cb_dc: i8,
    cr_count: u8,
    cr_dc: i8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SavedTileReference {
    tile_index: usize,
    framebuffer_offset: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SavedTileState {
    generation: u64,
    coefficients: Option<SavedCoefficientSeed>,
    reference: Option<SavedTileReference>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MvsSurfaceState {
    width: usize,
    height: usize,
    tiles_x: usize,
    tiles: Vec<SavedTileState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ModeFiveCoefficients {
    y: [i16; 64],
    cb: [i16; 64],
    cr: [i16; 64],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ModeFiveTileState {
    coefficients: ModeFiveCoefficients,
    seed: Option<SavedCoefficientSeed>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PartialOrderEvent {
    IdctScratch,
    CachePopulation,
}

#[cfg(test)]
thread_local! {
    static PARTIAL_ORDER_TRACE: std::cell::RefCell<Vec<PartialOrderEvent>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

#[cfg(test)]
fn record_partial_order_event(event: PartialOrderEvent) {
    PARTIAL_ORDER_TRACE.with(|trace| trace.borrow_mut().push(event));
}

#[cfg(not(test))]
fn record_partial_order_event(_event: PartialOrderEvent) {}

#[cfg(test)]
fn clear_partial_order_trace() {
    PARTIAL_ORDER_TRACE.with(|trace| trace.borrow_mut().clear());
}

#[cfg(test)]
fn take_partial_order_trace() -> Vec<PartialOrderEvent> {
    PARTIAL_ORDER_TRACE.with(|trace| std::mem::take(&mut *trace.borrow_mut()))
}

impl Default for ModeFiveCoefficients {
    fn default() -> Self {
        Self {
            y: [0; 64],
            cb: [0; 64],
            cr: [0; 64],
        }
    }
}

#[derive(Debug)]
pub struct PreparedMvsFull {
    decoded: DecodedMvsRect,
    next_decoder: MvsFullDecoder,
}

#[derive(Debug)]
pub(crate) struct PreparedMvsOpaqueState {
    next_decoder: MvsFullDecoder,
}

impl MvsFullDecoder {
    pub fn new(tables: MvsTables) -> Self {
        Self {
            tables,
            committed_records: 0,
            surface_state: None,
            cache_state: MvsCacheState::new(),
        }
    }

    pub fn prepare(
        &self,
        record: &MvsFullRecord<'_>,
        width: usize,
        height: usize,
    ) -> Result<PreparedMvsFull> {
        self.prepare_rect(record, 0, 0, width, height, width, height)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_rect(
        &self,
        record: &MvsFullRecord<'_>,
        rect_x: usize,
        rect_y: usize,
        width: usize,
        height: usize,
        surface_width: usize,
        surface_height: usize,
    ) -> Result<PreparedMvsFull> {
        let pixel_count = width
            .checked_mul(height)
            .context("MVS type-0 矩形像素数溢出")?;
        if pixel_count == 0 {
            bail!("MVS type-0 矩形尺寸不能为零");
        }
        if pixel_count > MAX_MVS_DECODE_PIXELS {
            bail!("MVS type-0 矩形超过协议解码预算: {pixel_count} 像素");
        }
        let surface_pixel_count = surface_width
            .checked_mul(surface_height)
            .context("MVS surface 像素数溢出")?;
        if surface_pixel_count == 0 {
            bail!("MVS surface 尺寸不能为零");
        }
        if surface_pixel_count > MAX_MVS_DECODE_PIXELS {
            bail!("MVS surface 超过协议解码预算: {surface_pixel_count} 像素");
        }
        let rect_right = rect_x
            .checked_add(width)
            .context("MVS type-0 矩形右边界溢出")?;
        let rect_bottom = rect_y
            .checked_add(height)
            .context("MVS type-0 矩形下边界溢出")?;
        if rect_right > surface_width || rect_bottom > surface_height {
            bail!(
                "MVS type-0 矩形超出 surface: rect=({rect_x},{rect_y},{width},{height}), surface={surface_width}x{surface_height}"
            );
        }
        let rgb_len = pixel_count
            .checked_mul(RGB_CHANNELS)
            .context("MVS type-0 RGB 缓冲区长度溢出")?;
        let tiles_x = width
            .checked_add(TILE_EDGE - 1)
            .context("MVS type-0 横向 tile 数溢出")?
            / TILE_EDGE;
        let tiles_y = height
            .checked_add(TILE_EDGE - 1)
            .context("MVS type-0 纵向 tile 数溢出")?
            / TILE_EDGE;
        let tile_count = tiles_x
            .checked_mul(tiles_y)
            .context("MVS type-0 tile 总数溢出")?;
        let surface_tiles_x = surface_width
            .checked_add(TILE_EDGE - 1)
            .context("MVS surface 横向 tile 数溢出")?
            / TILE_EDGE;
        let surface_tiles_y = surface_height
            .checked_add(TILE_EDGE - 1)
            .context("MVS surface 纵向 tile 数溢出")?
            / TILE_EDGE;
        let surface_tile_count = surface_tiles_x
            .checked_mul(surface_tiles_y)
            .context("MVS surface tile 总数溢出")?;
        let record_generation = self
            .committed_records
            .checked_add(1)
            .context("MVS type-0 已提交记录计数溢出")?;
        let mut surface_state = match &self.surface_state {
            Some(state)
                if state.width == surface_width
                    && state.height == surface_height
                    && state.tiles_x == surface_tiles_x =>
            {
                state.clone()
            }
            Some(_) => bail!("MVS surface 几何与 generation-scoped decoder 不一致"),
            None => MvsSurfaceState {
                width: surface_width,
                height: surface_height,
                tiles_x: surface_tiles_x,
                tiles: vec![SavedTileState::default(); surface_tile_count],
            },
        };
        let seed_origin_column = rect_x >> 3;
        let seed_origin_row = rect_y >> 3;

        let mut mode_bits = BitReader::new(record.mode_stream);
        let mut data_bits = BitReader::new(record.data_stream);
        mode_bits
            .read_bits(1)
            .context("MVS type-0 mode 流缺少起始位")?;

        let mut rgb = vec![0; rgb_len];
        let mut tile_index = 0usize;
        let mut one_color = [0, 0, 0];
        let mut two_colors = [[255, 255, 255], [181, 213, 254]];
        // Apple 在每条 type-0 record 开始时清除此状态；只有同一 record 内
        // 成功展开的 mode-5 tile 能成为下一 tile 的 predictor/copy 来源。
        let mut previous_mode_five = None;
        // previous cache index 也在整条 record 成功验证前只存于 staged clone。
        let mut cache_state = self.cache_state.clone();

        while tile_index < tile_count {
            let mode = u8::try_from(
                mode_bits
                    .read_bits(3)
                    .context("MVS type-0 mode 字段不完整")?,
            )?;
            let repeat =
                decode_repeat_count(&mut mode_bits).context("MVS type-0 repeat 字段不完整")?;
            let run_length = repeat
                .checked_add(1)
                .context("MVS type-0 tile run 长度溢出")?;
            let remaining = tile_count - tile_index;
            if run_length > remaining {
                bail!("MVS type-0 repeat 超过剩余 tile: run={run_length}, remaining={remaining}");
            }

            for _ in 0..run_length {
                let tile_column = tile_index % tiles_x;
                let tile_row = tile_index / tiles_x;
                let tile_x = tile_column
                    .checked_mul(TILE_EDGE)
                    .context("MVS type-0 tile x 坐标溢出")?;
                let tile_y = tile_row
                    .checked_mul(TILE_EDGE)
                    .context("MVS type-0 tile y 坐标溢出")?;
                let global_tile_column = seed_origin_column
                    .checked_add(tile_column)
                    .context("MVS type-0 全局 tile 列溢出")?;
                let global_tile_row = seed_origin_row
                    .checked_add(tile_row)
                    .context("MVS type-0 全局 tile 行溢出")?;
                let global_tile_index = global_tile_row
                    .checked_mul(surface_tiles_x)
                    .and_then(|value| value.checked_add(global_tile_column))
                    .context("MVS type-0 全局 tile 索引溢出")?;
                let global_tile = surface_state
                    .tiles
                    .get_mut(global_tile_index)
                    .context("MVS type-0 全局 tile 索引越界")?;
                global_tile.generation = record_generation;
                global_tile.reference = None;

                match mode {
                    0 => fill_tile(&mut rgb, width, height, tile_x, tile_y, [255; 3]),
                    1 => {
                        if tile_column == 0 {
                            bail!("MVS type-0 mode 1 不能出现在首列");
                        }
                        copy_tile(
                            &mut rgb,
                            width,
                            height,
                            tile_x - TILE_EDGE,
                            tile_y,
                            tile_x,
                            tile_y,
                        );
                        let source_tile_index = global_tile_index
                            .checked_sub(1)
                            .context("MVS type-0 mode 1 引用 tile 索引下溢")?;
                        let source_x = rect_x
                            .checked_add(tile_x - TILE_EDGE)
                            .context("MVS type-0 mode 1 引用 x 溢出")?;
                        let source_y = rect_y
                            .checked_add(tile_y)
                            .context("MVS type-0 mode 1 引用 y 溢出")?;
                        global_tile.reference = Some(SavedTileReference {
                            tile_index: source_tile_index,
                            framebuffer_offset: checked_framebuffer_offset(
                                surface_width,
                                source_x,
                                source_y,
                            )?,
                        });
                    }
                    2 => {
                        if tile_row == 0 {
                            bail!("MVS type-0 mode 2 不能出现在首行");
                        }
                        copy_tile(
                            &mut rgb,
                            width,
                            height,
                            tile_x,
                            tile_y - TILE_EDGE,
                            tile_x,
                            tile_y,
                        );
                        let source_tile_index = global_tile_index
                            .checked_sub(surface_tiles_x)
                            .context("MVS type-0 mode 2 引用 tile 索引下溢")?;
                        let source_x = rect_x
                            .checked_add(tile_x)
                            .context("MVS type-0 mode 2 引用 x 溢出")?;
                        let source_y = rect_y
                            .checked_add(tile_y - TILE_EDGE)
                            .context("MVS type-0 mode 2 引用 y 溢出")?;
                        global_tile.reference = Some(SavedTileReference {
                            tile_index: source_tile_index,
                            framebuffer_offset: checked_framebuffer_offset(
                                surface_width,
                                source_x,
                                source_y,
                            )?,
                        });
                    }
                    3 => decode_bitmap_tile(
                        &mut data_bits,
                        &mut rgb,
                        width,
                        height,
                        tile_x,
                        tile_y,
                        [255; 3],
                        [0; 3],
                    )?,
                    4 => {
                        let is_two_color = data_bits
                            .read_bits(1)
                            .context("MVS type-0 mode 4 缺少单双颜色标志")?
                            != 0;
                        let reuse_palette = data_bits
                            .read_bits(1)
                            .context("MVS type-0 mode 4 缺少颜色复用标志")?
                            != 0;
                        if is_two_color {
                            if !reuse_palette {
                                two_colors[0] = read_mode_four_color(&mut data_bits)?;
                                two_colors[1] = read_mode_four_color(&mut data_bits)?;
                            }
                            decode_bitmap_tile(
                                &mut data_bits,
                                &mut rgb,
                                width,
                                height,
                                tile_x,
                                tile_y,
                                two_colors[0],
                                two_colors[1],
                            )?;
                        } else {
                            if !reuse_palette {
                                one_color = read_mode_four_color(&mut data_bits)?;
                            }
                            fill_tile(&mut rgb, width, height, tile_x, tile_y, one_color);
                        }
                    }
                    5 => {
                        let tile_state = decode_mode_five_coefficients(
                            &mut data_bits,
                            previous_mode_five.as_ref(),
                            record.scale_threshold_a,
                            record.scale_threshold_b,
                        )?;
                        render_mode_five_tile(
                            &mut rgb,
                            width,
                            height,
                            tile_x,
                            tile_y,
                            &tile_state.coefficients,
                            &self.tables,
                        )?;
                        if let Some(seed) = tile_state.seed {
                            global_tile.coefficients = Some(seed);
                        }
                        previous_mode_five = Some(tile_state);
                    }
                    6 | 7 => {
                        let cache_index = if mode == 6 {
                            let high = u16::from(
                                data_bits
                                    .read_u8()
                                    .context("MVS type-0 mode 6 缺少 cache 索引高字节")?,
                            );
                            let low = u16::from(
                                data_bits
                                    .read_u8()
                                    .context("MVS type-0 mode 6 缺少 cache 索引低字节")?,
                            );
                            (high << 8) | low
                        } else {
                            cache_state.previous_cache_index.wrapping_add(1)
                        };
                        let entry = lookup_cache_entry(&cache_state, cache_index)?;
                        let coefficients = cache_entry_coefficients(&entry);
                        render_mode_five_tile(
                            &mut rgb,
                            width,
                            height,
                            tile_x,
                            tile_y,
                            &coefficients,
                            &self.tables,
                        )?;
                        cache_state.previous_cache_index = cache_index;
                    }
                    _ => unreachable!("three-bit mode is always in 0..=7"),
                }
                tile_index += 1;
            }
        }

        let mode_terminal = mode_bits
            .read_u8()
            .context("MVS type-0 mode 流缺少终止符")?;
        if mode_terminal != STREAM_TERMINAL {
            bail!("MVS type-0 mode 流终止符非法: {mode_terminal:#04x}");
        }
        let data_terminal = data_bits
            .read_u8()
            .context("MVS type-0 data 流缺少终止符")?;
        if data_terminal != STREAM_TERMINAL {
            bail!("MVS type-0 data 流终止符非法: {data_terminal:#04x}");
        }

        let mut next_decoder = self.clone();
        next_decoder.committed_records = record_generation;
        next_decoder.surface_state = Some(surface_state);
        next_decoder.cache_state = cache_state;
        Ok(PreparedMvsFull {
            decoded: DecodedMvsRect { width, height, rgb },
            next_decoder,
        })
    }

    pub fn decoded(prepared: &PreparedMvsFull) -> &DecodedMvsRect {
        &prepared.decoded
    }

    pub fn commit(&mut self, prepared: PreparedMvsFull) -> DecodedMvsRect {
        *self = prepared.next_decoder;
        prepared.decoded
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_partial(
        &self,
        payload: &[u8],
        rect_x: usize,
        rect_y: usize,
        width: usize,
        height: usize,
        surface_width: usize,
        surface_height: usize,
    ) -> Result<PreparedMvsOpaqueState> {
        if payload.len() < 3 || payload[0] != 1 {
            bail!("MVS type-1 payload 头部非法");
        }
        if width == 0 || height == 0 || surface_width == 0 || surface_height == 0 {
            bail!("MVS type-1 矩形与 surface 尺寸不能为零");
        }
        let rect_right = rect_x
            .checked_add(width)
            .context("MVS type-1 矩形右边界溢出")?;
        let rect_bottom = rect_y
            .checked_add(height)
            .context("MVS type-1 矩形下边界溢出")?;
        if rect_right > surface_width || rect_bottom > surface_height {
            bail!("MVS type-1 矩形超出当前 surface");
        }
        let surface = self
            .surface_state
            .as_ref()
            .context("MVS type-1 缺少已提交的 full-frame tile 状态")?;
        if surface.width != surface_width || surface.height != surface_height {
            bail!("MVS type-1 surface 几何与 generation-scoped decoder 不一致");
        }
        let tiles_x = width
            .checked_add(TILE_EDGE - 1)
            .context("MVS type-1 横向 tile 数溢出")?
            / TILE_EDGE;
        let tiles_y = height
            .checked_add(TILE_EDGE - 1)
            .context("MVS type-1 纵向 tile 数溢出")?
            / TILE_EDGE;
        let tile_count = tiles_x
            .checked_mul(tiles_y)
            .context("MVS type-1 tile 总数溢出")?;
        let origin_column = rect_x >> 3;
        let origin_row = rect_y >> 3;
        let cb_extent = usize::from(payload[1].min(64));
        let cr_extent = usize::from(payload[2].min(64));
        let mut reader = BitReader::new(&payload[3..]);
        let mut next_decoder = self.clone();

        for local_tile_index in 0..tile_count {
            let tile_column = local_tile_index % tiles_x;
            let tile_row = local_tile_index / tiles_x;
            let global_column = origin_column
                .checked_add(tile_column)
                .context("MVS type-1 全局 tile 列溢出")?;
            let global_row = origin_row
                .checked_add(tile_row)
                .context("MVS type-1 全局 tile 行溢出")?;
            let global_tile_index = global_row
                .checked_mul(surface.tiles_x)
                .and_then(|value| value.checked_add(global_column))
                .context("MVS type-1 全局 tile 索引溢出")?;
            let tile = surface
                .tiles
                .get(global_tile_index)
                .context("MVS type-1 全局 tile 索引越界")?;
            let opcode_start_bit = reader.bit_position();
            let opcode_result = reader.read_bits(2).context("MVS type-1 opcode 不完整");
            let opcode = opcode_result.with_context(|| {
                format!(
                    "MVS type-1 opcode 读取失败: rect=({rect_x},{rect_y},{width},{height}) local_index={local_tile_index} local_row={tile_row} local_col={tile_column} global_index={global_tile_index} global_row={global_row} global_col={global_column} opcode_start_bit={opcode_start_bit} failure_bit={}",
                    reader.bit_position()
                )
            })?;
            let tile_result = (|| -> Result<()> {
                match opcode {
                    0 => {}
                    1 => {
                        let seed = tile
                            .coefficients
                            .as_ref()
                            .context("MVS type-1 opcode 1 缺少 full-frame 系数 seed")?;
                        let entry = prepare_partial_cache_entry(
                            &mut reader,
                            seed,
                            cb_extent,
                            cr_extent,
                            &self.tables,
                        )?;
                        stage_cache_population(&mut next_decoder.cache_state, entry)?;
                    }
                    2 => validate_saved_reference(surface, tile)?,
                    3 => {
                        let index = if reader
                            .read_bits(1)
                            .context("MVS type-1 opcode 3 缺少 selector")?
                            != 0
                        {
                            next_decoder
                                .cache_state
                                .previous_cache_index
                                .wrapping_add(1)
                        } else {
                            let high = u16::from(
                                reader.read_u8().context("MVS type-1 opcode 3 缺少高字节")?,
                            );
                            let low = u16::from(
                                reader.read_u8().context("MVS type-1 opcode 3 缺少低字节")?,
                            );
                            (high << 8) | low
                        };
                        // Apple ignores `_Cache_UpdateTile`'s return value on the
                        // type-1 opcode-3 path. A bounded lookup miss is therefore
                        // a pixel-opaque no-op; only a hit runs scratch IDCT and
                        // advances the previous-index state. Rust still rejects
                        // unsafe indices inside `lookup_cache_entry` without ever
                        // indexing the fixed cache.
                        if let Ok(entry) = lookup_cache_entry(&next_decoder.cache_state, index) {
                            run_cache_entry_idct_scratch(&entry, &self.tables)?;
                            next_decoder.cache_state.previous_cache_index = index;
                        }
                    }
                    _ => unreachable!("two-bit opcode is always in 0..=3"),
                }
                Ok(())
            })();
            tile_result.with_context(|| {
                format!(
                    "MVS type-1 tile 处理失败: rect=({rect_x},{rect_y},{width},{height}) local_index={local_tile_index} local_row={tile_row} local_col={tile_column} global_index={global_tile_index} global_row={global_row} global_col={global_column} opcode={opcode} opcode_start_bit={opcode_start_bit} failure_bit={}",
                    reader.bit_position()
                )
            })?;
        }

        for (terminal_index, expected) in [0x6d, 0x76, 0x73].into_iter().enumerate() {
            let terminal_bit = reader.bit_position();
            let terminal_result = (|| -> Result<()> {
                let actual = reader.read_u8().context("MVS type-1 位流缺少终止符")?;
                if actual != expected {
                    bail!("MVS type-1 位流终止符非法: {actual:#04x}");
                }
                Ok(())
            })();
            terminal_result.with_context(|| {
                format!(
                    "MVS type-1 terminal 失败: rect=({rect_x},{rect_y},{width},{height}) terminal_index={terminal_index} terminal_bit={terminal_bit} failure_bit={}",
                    reader.bit_position()
                )
            })?;
        }

        Ok(PreparedMvsOpaqueState { next_decoder })
    }

    pub(crate) fn commit_opaque(&mut self, prepared: PreparedMvsOpaqueState) {
        *self = prepared.next_decoder;
    }
}

fn decode_huffman_token(reader: &mut BitReader<'_>, table: &AppleHuffmanTable) -> Result<u8> {
    let mut code = 0usize;
    let mut first_code = 0usize;
    let mut value_offset = 0usize;
    for (length_index, &count_byte) in table.bits.iter().enumerate() {
        code =
            code.checked_shl(1)
                .context("MVS type-1 Huffman code 溢出")?
                | usize::try_from(reader.read_bits(1).with_context(|| {
                    format!("MVS type-1 Huffman 第 {} 位耗尽", length_index + 1)
                })?)?;
        let count = usize::from(count_byte);
        let end_code = first_code
            .checked_add(count)
            .context("MVS type-1 Huffman code range 溢出")?;
        if code >= first_code && code < end_code {
            let index = value_offset
                .checked_add(code - first_code)
                .context("MVS type-1 Huffman value 索引溢出")?;
            return table
                .values
                .get(index)
                .copied()
                .context("MVS type-1 Huffman value 索引越界");
        }
        value_offset = value_offset
            .checked_add(count)
            .context("MVS type-1 Huffman value offset 溢出")?;
        first_code = end_code
            .checked_shl(1)
            .context("MVS type-1 Huffman first code 溢出")?;
    }
    bail!("MVS type-1 Huffman code 无匹配 token")
}

fn decode_partial_ac(
    reader: &mut BitReader<'_>,
    coefficients: &mut [i16; 64],
    start: usize,
    extent: usize,
    table: &AppleHuffmanTable,
) -> Result<()> {
    let mut cursor = 0usize;
    while cursor < extent {
        let token = decode_huffman_token(reader, table)?;
        let run = usize::from(token >> 4);
        let size = token & 0x0f;
        if size == 0 {
            if run != 15 {
                break;
            }
            cursor = cursor
                .checked_add(16)
                .context("MVS type-1 Huffman ZRL 位置溢出")?;
            continue;
        }

        let target = cursor
            .checked_add(run)
            .context("MVS type-1 Huffman run 位置溢出")?;
        let raw = i32::try_from(
            reader
                .read_bits(size)
                .context("MVS type-1 Huffman level 不完整")?,
        )?;
        let sign_boundary = 1i32
            .checked_shl(u32::from(size - 1))
            .context("MVS type-1 Huffman sign boundary 溢出")?;
        let level = if raw < sign_boundary {
            raw.checked_add(1)
                .and_then(|value| value.checked_sub(1i32.checked_shl(u32::from(size))?))
                .context("MVS type-1 Huffman 负 level 溢出")?
        } else {
            raw
        };
        let scan_index = start
            .checked_add(target)
            .context("MVS type-1 Huffman scan 索引溢出")?;
        let natural_index = *APPLE_NATURAL_ORDER
            .get(scan_index)
            .context("MVS type-1 Huffman scan 索引越界")?;
        coefficients[natural_index] = i16::try_from(level)?;
        cursor = target
            .checked_add(1)
            .context("MVS type-1 Huffman 位置溢出")?;
    }
    Ok(())
}

fn refine_saved_zero(reader: &mut BitReader<'_>) -> Result<i8> {
    if reader
        .read_bits(1)
        .context("MVS type-1 saved-zero 缺少 present 位")?
        == 0
    {
        return Ok(0);
    }
    if reader
        .read_bits(1)
        .context("MVS type-1 saved-zero 缺少 sign 位")?
        == 0
    {
        Ok(1)
    } else {
        Ok(-1)
    }
}

fn refine_saved_byte(reader: &mut BitReader<'_>, saved: i8, width: u8) -> Result<i8> {
    if saved == 0 {
        return Ok(decode_dc_rice(reader).context("MVS type-1 saved-zero Rice 解码失败")? as i8);
    }
    let delta = u8::try_from(
        reader
            .read_bits(width)
            .context("MVS type-1 saved coefficient delta 不完整")?,
    )? as i8;
    Ok(if saved.is_negative() {
        saved.wrapping_sub(delta)
    } else {
        saved.wrapping_add(delta)
    })
}

fn refine_saved_y(reader: &mut BitReader<'_>, seed: &SavedCoefficientSeed) -> Result<[i8; 64]> {
    let extent = usize::try_from(
        reader
            .read_bits(6)
            .context("MVS type-1 opcode 1 缺少 Y extent")?,
    )?
    .checked_add(1)
    .context("MVS type-1 Y extent 溢出")?;
    let threshold = usize::from(seed.selected_threshold);
    let mut saved = [0i8; 64];
    saved[0] = seed.y[0];
    let first_end = extent.min(threshold);

    if threshold < 15 {
        for (scan, output) in saved.iter_mut().enumerate().take(first_end).skip(1) {
            *output = refine_saved_byte(reader, seed.y[scan], 3)?;
        }
        for (scan, output) in saved
            .iter_mut()
            .enumerate()
            .take(extent)
            .skip(threshold.max(1))
        {
            *output = refine_saved_byte(reader, seed.y[scan], 4)?;
        }
    } else {
        for (scan, output) in saved.iter_mut().enumerate().take(first_end).skip(1) {
            let original = seed.y[scan];
            *output = if original == 0 {
                refine_saved_zero(reader)?
            } else {
                let step = i8::try_from(
                    reader
                        .read_bits(1)
                        .context("MVS type-1 saved coefficient one-bit delta 不完整")?,
                )?;
                if original.is_negative() {
                    original.wrapping_sub(step)
                } else {
                    original.wrapping_add(step)
                }
            };
        }
        for (scan, output) in saved
            .iter_mut()
            .enumerate()
            .take(extent)
            .skip(threshold.max(1))
        {
            *output = refine_saved_byte(reader, seed.y[scan], 3)?;
        }
    }
    Ok(saved)
}

fn refine_saved_chroma_dc(reader: &mut BitReader<'_>, saved: i8) -> Result<i16> {
    if saved == 0 {
        return Ok(i16::from(refine_saved_zero(reader)?));
    }
    let step = i8::try_from(
        reader
            .read_bits(1)
            .context("MVS type-1 chroma DC delta 不完整")?,
    )?;
    Ok(i16::from(if saved.is_negative() {
        saved.wrapping_sub(step)
    } else {
        saved.wrapping_add(step)
    }))
}

fn prepare_partial_cache_entry(
    reader: &mut BitReader<'_>,
    seed: &SavedCoefficientSeed,
    cb_extent: usize,
    cr_extent: usize,
    tables: &MvsTables,
) -> Result<[i8; MVS_CACHE_ENTRY_BYTES]> {
    let saved_y = refine_saved_y(reader, seed)?;
    if seed.cb_count != 1 || seed.cr_count != 1 {
        bail!("MVS type-1 opcode 1 saved Cb/Cr count 必须均为 1");
    }

    let mut coefficients = ModeFiveCoefficients::default();
    for (scan, &value) in saved_y.iter().enumerate() {
        coefficients.y[APPLE_NATURAL_ORDER[scan]] = i16::from(value);
    }
    coefficients.cb[0] = refine_saved_chroma_dc(reader, seed.cb_dc)?;
    decode_partial_ac(
        reader,
        &mut coefficients.cb,
        1,
        cb_extent,
        &APPLE_CHROMA_AC_HUFFMAN,
    )?;
    coefficients.cr[0] = refine_saved_chroma_dc(reader, seed.cr_dc)?;
    decode_partial_ac(
        reader,
        &mut coefficients.cr,
        1,
        cr_extent,
        &APPLE_CHROMA_AC_HUFFMAN,
    )?;

    run_partial_idct_scratch(&coefficients, tables)?;

    let mut entry = [0i8; MVS_CACHE_ENTRY_BYTES];
    entry[..64].copy_from_slice(&saved_y);
    for scan in 0..15 {
        entry[64 + scan] = coefficients.cb[APPLE_NATURAL_ORDER[scan]] as i8;
    }
    for scan in 0..20 {
        entry[79 + scan] = coefficients.cr[APPLE_NATURAL_ORDER[scan]] as i8;
    }
    Ok(entry)
}

fn run_partial_idct_scratch(coefficients: &ModeFiveCoefficients, tables: &MvsTables) -> Result<()> {
    let y = inverse_dct_8x8(&coefficients.y, &tables.luminance)
        .context("MVS type-1 Y IDCT scratch 失败")?;
    let cb = inverse_dct_8x8(&coefficients.cb, &tables.chrominance)
        .context("MVS type-1 Cb IDCT scratch 失败")?;
    let cr = inverse_dct_8x8(&coefficients.cr, &tables.chrominance)
        .context("MVS type-1 Cr IDCT scratch 失败")?;
    let mut scratch = [0u8; TILE_EDGE * TILE_EDGE * RGB_CHANNELS];
    for index in 0..TILE_EDGE * TILE_EDGE {
        let color = ycbcr_8_to_rgb(y[index], cb[index], cr[index]);
        let offset = index * RGB_CHANNELS;
        scratch[offset..offset + RGB_CHANNELS].copy_from_slice(&color);
    }
    record_partial_order_event(PartialOrderEvent::IdctScratch);
    Ok(())
}

fn stage_cache_population(
    cache: &mut MvsCacheState,
    entry: [i8; MVS_CACHE_ENTRY_BYTES],
) -> Result<()> {
    let next_count = cache
        .population_count
        .checked_add(1)
        .context("MVS cache population counter 溢出")?;
    let next_index = match cache.last_insert_index {
        MVS_CACHE_LAST_USABLE_INDEX => 1,
        index if index < MVS_CACHE_LAST_USABLE_INDEX => index + 1,
        index => bail!("MVS cache last insertion index 非法: {index}"),
    };
    cache.population_count = next_count;
    cache.last_insert_index = next_index;
    let entries = Arc::make_mut(&mut cache.entries);
    entries[usize::from(next_index)] = Some(Arc::new(entry));
    record_partial_order_event(PartialOrderEvent::CachePopulation);
    Ok(())
}

fn lookup_cache_entry(
    cache: &MvsCacheState,
    index: u16,
) -> Result<Arc<[i8; MVS_CACHE_ENTRY_BYTES]>> {
    if index == 0 || usize::from(index) >= MVS_CACHE_SLOT_COUNT {
        bail!("MVS cache 索引越界: {index}");
    }
    if cache.population_count < u32::from(index) {
        bail!("MVS cache population counter 小于查询索引: {index}");
    }
    cache
        .entries
        .get(usize::from(index))
        .and_then(Option::as_ref)
        .cloned()
        .with_context(|| format!("MVS cache 索引未初始化: {index}"))
}

fn cache_entry_coefficients(entry: &[i8; MVS_CACHE_ENTRY_BYTES]) -> ModeFiveCoefficients {
    let mut coefficients = ModeFiveCoefficients::default();
    for scan in 0..64 {
        coefficients.y[APPLE_NATURAL_ORDER[scan]] = i16::from(entry[scan]);
    }
    for scan in 0..15 {
        coefficients.cb[APPLE_NATURAL_ORDER[scan]] = i16::from(entry[64 + scan]);
    }
    for scan in 0..20 {
        coefficients.cr[APPLE_NATURAL_ORDER[scan]] = i16::from(entry[79 + scan]);
    }
    coefficients
}

fn run_cache_entry_idct_scratch(
    entry: &[i8; MVS_CACHE_ENTRY_BYTES],
    tables: &MvsTables,
) -> Result<()> {
    run_partial_idct_scratch(&cache_entry_coefficients(entry), tables)
}

fn validate_saved_reference(surface: &MvsSurfaceState, tile: &SavedTileState) -> Result<()> {
    // Apple treats an absent saved reference, or a reference whose source
    // generation no longer matches, as a pixel-opaque no-op and continues the
    // type-1 record. A present matching reference is still bounds-validated.
    let Some(reference) = tile.reference.as_ref() else {
        return Ok(());
    };
    let source = surface
        .tiles
        .get(reference.tile_index)
        .context("MVS type-1 opcode 2 saved reference tile 越界")?;
    if source.generation != tile.generation {
        return Ok(());
    }
    let framebuffer_bytes = surface
        .width
        .checked_mul(surface.height)
        .and_then(|value| value.checked_mul(APPLE_FRAMEBUFFER_PIXEL_BYTES))
        .context("MVS type-1 framebuffer 长度溢出")?;
    if reference.framebuffer_offset >= framebuffer_bytes {
        bail!("MVS type-1 opcode 2 framebuffer offset 越界");
    }
    Ok(())
}

fn decode_unary_ones(
    reader: &mut BitReader<'_>,
    maximum: usize,
    field_name: &str,
) -> Result<usize> {
    let mut ones = 0usize;
    loop {
        let bit = reader
            .read_bits(1)
            .with_context(|| format!("MVS mode 5 {field_name} unary 字段不完整"))?;
        if bit == 0 {
            return Ok(ones);
        }
        ones = ones
            .checked_add(1)
            .with_context(|| format!("MVS mode 5 {field_name} unary 计数溢出"))?;
        if ones > maximum {
            bail!("MVS mode 5 {field_name} unary 超过固定上限 {maximum}");
        }
    }
}

fn signed_magnitude(magnitude: i32, negative: bool, field_name: &str) -> Result<i16> {
    let signed = if negative {
        magnitude
            .checked_neg()
            .with_context(|| format!("MVS mode 5 {field_name} 符号转换溢出"))?
    } else {
        magnitude
    };
    i16::try_from(signed).with_context(|| format!("MVS mode 5 {field_name} 超出 i16"))
}

fn decode_dc_rice(reader: &mut BitReader<'_>) -> Result<i16> {
    let quotient = decode_unary_ones(reader, DC_MAX_UNARY_ONES, "DC")?;
    if quotient == 0 {
        let present = reader
            .read_bits(1)
            .context("MVS mode 5 DC q0 缺少 present 位")?;
        if present == 0 {
            return Ok(0);
        }
        let negative = reader.read_bits(1).context("MVS mode 5 DC q0 缺少符号位")? != 0;
        return signed_magnitude(1, negative, "DC q0");
    }

    let remainder_width = u8::try_from(quotient.min(3))?;
    let remainder = i32::try_from(
        reader
            .read_bits(remainder_width)
            .context("MVS mode 5 DC remainder 字段不完整")?,
    )?;
    let base = match quotient {
        1 => 2,
        2 => 4,
        _ => i32::try_from(
            quotient
                .checked_sub(2)
                .and_then(|value| value.checked_mul(8))
                .context("MVS mode 5 DC magnitude 基数溢出")?,
        )?,
    };
    let magnitude = base
        .checked_add(remainder)
        .context("MVS mode 5 DC magnitude 溢出")?;
    let negative = reader.read_bits(1).context("MVS mode 5 DC 缺少符号位")? != 0;
    signed_magnitude(magnitude, negative, "DC")
}

fn luma_ac_shift(index: usize, threshold: u8) -> Result<u32> {
    if !(1..64).contains(&index) {
        bail!("MVS mode 5 AC 索引超界: {index}");
    }
    let threshold = usize::from(threshold);
    Ok(if threshold < 15 {
        if index < threshold {
            3
        } else {
            4
        }
    } else if index <= 5 || index < threshold {
        1
    } else {
        3
    })
}

fn scaled_ac_value(magnitude: i32, negative: bool, shift: u32) -> Result<i16> {
    let scaled = magnitude
        .checked_shl(shift)
        .context("MVS mode 5 AC 系数左移溢出")?;
    signed_magnitude(scaled, negative, "AC")
}

fn decode_ac_unary_ones(reader: &mut BitReader<'_>, first_band: bool) -> Result<Option<usize>> {
    if first_band {
        return decode_unary_ones(reader, FIRST_AC_MAX_UNARY_ONES, "低频 AC").map(Some);
    }

    let mut ones = 0usize;
    loop {
        let bit = reader
            .read_bits(1)
            .context("MVS mode 5 高频 AC unary 字段不完整")?;
        if bit == 0 {
            return Ok(Some(ones));
        }
        ones = ones
            .checked_add(1)
            .context("MVS mode 5 高频 AC unary 计数溢出")?;
        if ones == SECOND_AC_MAX_UNARY_ONES + 1 {
            // Apple 在 0x14 guard 上记录诊断后返回成功，且不会消费 unary
            // 终止位，也不会运行辅助 saved-state finalizer。调用者仍使用已清零
            // 并部分填充的系数缓冲，因此 Rust 返回当前 staged 系数即可。
            return Ok(None);
        }
    }
}

fn decode_luma_ac_rice(reader: &mut BitReader<'_>, threshold: u8) -> Result<[i16; 64]> {
    let mut coefficients = [0i16; 64];
    let mut index = 1usize;
    while index < 64 {
        let prefix = reader
            .read_bits(1)
            .context("MVS mode 5 AC 缺少 coefficient prefix")?;
        if prefix == 0 {
            let token = reader
                .read_bits(2)
                .context("MVS mode 5 AC token 字段不完整")?;
            match token {
                0 => {
                    index += 1;
                }
                1 => {
                    let run_selected = reader
                        .read_bits(1)
                        .context("MVS mode 5 AC EOB/run 位不完整")?;
                    if run_selected == 0 {
                        break;
                    }
                    let group = usize::try_from(
                        reader
                            .read_bits(2)
                            .context("MVS mode 5 AC run 选择字段不完整")?,
                    )?;
                    if group < 3 {
                        index = index
                            .checked_add(3 + group)
                            .context("MVS mode 5 AC 短 run 索引溢出")?;
                        continue;
                    }

                    let mut advance = 6usize;
                    loop {
                        let delta = usize::try_from(
                            reader
                                .read_bits(3)
                                .context("MVS mode 5 AC 扩展 run 字段不完整")?,
                        )?;
                        advance = advance
                            .checked_add(delta)
                            .context("MVS mode 5 AC 扩展 run 计数溢出")?;
                        let landing = index
                            .checked_add(advance)
                            .context("MVS mode 5 AC 扩展 run 索引溢出")?;
                        if landing >= 64 {
                            bail!("MVS mode 5 AC 扩展 run 超过 coefficient 63");
                        }
                        if delta != 7 {
                            index = landing;
                            break;
                        }
                    }
                }
                2 | 3 => {
                    let shift = luma_ac_shift(index, threshold)?;
                    coefficients[APPLE_NATURAL_ORDER[index]] =
                        scaled_ac_value(1, token == 3, shift)?;
                    index += 1;
                }
                _ => unreachable!("two-bit token is in 0..=3"),
            }
            continue;
        }

        let first_band = index <= 5;
        let Some(quotient) = decode_ac_unary_ones(reader, first_band)? else {
            return Ok(coefficients);
        };
        let remainder_width = if first_band {
            if quotient < 4 {
                2
            } else {
                3
            }
        } else {
            match quotient {
                0 => 1,
                1 => 2,
                _ => 3,
            }
        };
        let remainder = i32::try_from(
            reader
                .read_bits(remainder_width)
                .context("MVS mode 5 AC remainder 字段不完整")?,
        )?;
        let quotient_i32 = i32::try_from(quotient)?;
        let magnitude = if first_band {
            if quotient < 4 {
                quotient_i32
                    .checked_mul(4)
                    .and_then(|value| value.checked_add(remainder))
                    .and_then(|value| value.checked_add(2))
            } else {
                quotient_i32
                    .checked_mul(8)
                    .and_then(|value| value.checked_add(remainder))
                    .and_then(|value| value.checked_sub(14))
            }
        } else {
            match quotient {
                0 => remainder.checked_add(2),
                1 => remainder.checked_add(4),
                _ => quotient_i32
                    .checked_mul(8)
                    .and_then(|value| value.checked_add(remainder))
                    .and_then(|value| value.checked_sub(8)),
            }
        }
        .context("MVS mode 5 AC magnitude 溢出")?;
        let negative = reader.read_bits(1).context("MVS mode 5 AC 缺少符号位")? != 0;
        coefficients[APPLE_NATURAL_ORDER[index]] =
            scaled_ac_value(magnitude, negative, luma_ac_shift(index, threshold)?)?;
        index += 1;
    }
    Ok(coefficients)
}

fn apply_luma_dc_predictor(previous: i16, delta: i16) -> Result<i16> {
    previous
        .checked_sub(delta)
        .context("MVS mode 5 Y DC predictor 溢出")
}

fn apply_chroma_dc_predictor(previous: i16, delta: i16) -> Result<i16> {
    let half_toward_zero = previous / 2;
    half_toward_zero
        .checked_sub(delta)
        .and_then(|value| value.checked_mul(2))
        .context("MVS mode 5 Cb/Cr DC predictor 溢出")
}

fn decode_mode_five_coefficients(
    reader: &mut BitReader<'_>,
    previous: Option<&ModeFiveTileState>,
    scale_threshold_a: u8,
    scale_threshold_b: u8,
) -> Result<ModeFiveTileState> {
    let copy_previous = reader.read_bits(1).context("MVS mode 5 缺少 copy 标志")? != 0;
    if copy_previous {
        // `_ExpandBlockRice` 对首个 tile 的无来源 copy 明确记录诊断、
        // 清零 3×64 系数并返回成功；此兼容规则仅属于 mode 5。
        return Ok(previous.copied().unwrap_or(ModeFiveTileState {
            coefficients: ModeFiveCoefficients::default(),
            seed: None,
        }));
    }

    let reuse_chroma = reader
        .read_bits(1)
        .context("MVS mode 5 缺少 Cb/Cr predictor 复用标志")?
        != 0;
    let select_threshold_b = reader
        .read_bits(1)
        .context("MVS mode 5 缺少 scale threshold 选择位")?
        != 0;
    let selected_threshold = if select_threshold_b {
        scale_threshold_b
    } else {
        scale_threshold_a
    };

    let prior = previous.map(|state| state.coefficients).unwrap_or_default();
    let mut current = ModeFiveCoefficients::default();
    if reuse_chroma {
        current.cb[0] = prior.cb[0];
        current.cr[0] = prior.cr[0];
    } else {
        let cb_delta = decode_dc_rice(reader).context("MVS mode 5 Cb DC 解码失败")?;
        current.cb[0] = apply_chroma_dc_predictor(prior.cb[0], cb_delta)?;
        let cr_delta = decode_dc_rice(reader).context("MVS mode 5 Cr DC 解码失败")?;
        current.cr[0] = apply_chroma_dc_predictor(prior.cr[0], cr_delta)?;
    }

    let y_delta = decode_dc_rice(reader).context("MVS mode 5 Y DC 解码失败")?;
    current.y =
        decode_luma_ac_rice(reader, selected_threshold).context("MVS mode 5 Y AC 解码失败")?;
    current.y[0] = apply_luma_dc_predictor(prior.y[0], y_delta)?;
    let mut saved_y = [0i8; 64];
    for scan_index in 0..64 {
        saved_y[scan_index] = current.y[APPLE_NATURAL_ORDER[scan_index]] as i8;
    }
    Ok(ModeFiveTileState {
        coefficients: current,
        seed: Some(SavedCoefficientSeed {
            selected_threshold,
            y: saved_y,
            cb_count: 1,
            cb_dc: current.cb[0] as i8,
            cr_count: 1,
            cr_dc: current.cr[0] as i8,
        }),
    })
}

fn checked_framebuffer_offset(surface_width: usize, x: usize, y: usize) -> Result<usize> {
    y.checked_mul(surface_width)
        .and_then(|value| value.checked_add(x))
        .and_then(|value| value.checked_mul(APPLE_FRAMEBUFFER_PIXEL_BYTES))
        .context("MVS framebuffer 引用偏移溢出")
}

#[allow(clippy::too_many_arguments)]
fn render_mode_five_tile(
    rgb: &mut [u8],
    width: usize,
    height: usize,
    tile_x: usize,
    tile_y: usize,
    coefficients: &ModeFiveCoefficients,
    tables: &MvsTables,
) -> Result<()> {
    let y =
        inverse_dct_8x8(&coefficients.y, &tables.luminance).context("MVS mode 5 Y IDCT 失败")?;
    let cb = inverse_dct_8x8(&coefficients.cb, &tables.chrominance)
        .context("MVS mode 5 Cb IDCT 失败")?;
    let cr = inverse_dct_8x8(&coefficients.cr, &tables.chrominance)
        .context("MVS mode 5 Cr IDCT 失败")?;

    for dy in 0..TILE_EDGE {
        for dx in 0..TILE_EDGE {
            let logical = dy * TILE_EDGE + dx;
            let color = ycbcr_8_to_rgb(y[logical], cb[logical], cr[logical]);
            if tile_x + dx < width && tile_y + dy < height {
                write_pixel(rgb, width, tile_x + dx, tile_y + dy, color);
            }
        }
    }
    Ok(())
}

fn fill_tile(
    rgb: &mut [u8],
    width: usize,
    height: usize,
    tile_x: usize,
    tile_y: usize,
    color: [u8; 3],
) {
    for dy in 0..TILE_EDGE.min(height - tile_y) {
        for dx in 0..TILE_EDGE.min(width - tile_x) {
            write_pixel(rgb, width, tile_x + dx, tile_y + dy, color);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_tile(
    rgb: &mut [u8],
    width: usize,
    height: usize,
    source_x: usize,
    source_y: usize,
    tile_x: usize,
    tile_y: usize,
) {
    for dy in 0..TILE_EDGE.min(height - tile_y) {
        for dx in 0..TILE_EDGE.min(width - tile_x) {
            let source = pixel_offset(width, source_x + dx, source_y + dy);
            let color = [rgb[source], rgb[source + 1], rgb[source + 2]];
            write_pixel(rgb, width, tile_x + dx, tile_y + dy, color);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_bitmap_tile(
    data_bits: &mut BitReader<'_>,
    rgb: &mut [u8],
    width: usize,
    height: usize,
    tile_x: usize,
    tile_y: usize,
    one: [u8; 3],
    zero: [u8; 3],
) -> Result<()> {
    let row_mask = data_bits
        .read_u8()
        .context("MVS type-0 bitmap 缺少行掩码")?;
    for dy in 0..TILE_EDGE {
        let solid_one = row_mask & (0x80 >> dy) != 0;
        let pixel_mask = if solid_one {
            0xff
        } else {
            data_bits
                .read_u8()
                .context("MVS type-0 bitmap 缺少像素掩码")?
        };
        for dx in 0..TILE_EDGE {
            if tile_x + dx >= width || tile_y + dy >= height {
                continue;
            }
            let color = if pixel_mask & (0x80 >> dx) != 0 {
                one
            } else {
                zero
            };
            write_pixel(rgb, width, tile_x + dx, tile_y + dy, color);
        }
    }
    Ok(())
}

fn read_mode_four_color(data_bits: &mut BitReader<'_>) -> Result<[u8; 3]> {
    let y = data_bits
        .read_u8()
        .context("MVS type-0 mode 4 缺少 Y 字段")?;
    let cb = u8::try_from(
        data_bits
            .read_bits(6)
            .context("MVS type-0 mode 4 缺少 Cb 字段")?,
    )?;
    let cr = u8::try_from(
        data_bits
            .read_bits(6)
            .context("MVS type-0 mode 4 缺少 Cr 字段")?,
    )?;
    Ok(ycbcr_20_to_rgb(y, cb, cr))
}

const IDCT_CONST_BITS: u32 = 13;
const IDCT_PASS1_BITS: u32 = 2;
const FIX_0_298631336: i64 = 2_446;
const FIX_0_390180644: i64 = 3_196;
const FIX_0_541196100: i64 = 4_433;
const FIX_0_765366865: i64 = 6_270;
const FIX_0_899976223: i64 = 7_373;
const FIX_1_175875602: i64 = 9_633;
const FIX_1_501321110: i64 = 12_299;
const FIX_1_847759065: i64 = 15_137;
const FIX_1_961570560: i64 = 16_069;
const FIX_2_053119869: i64 = 16_819;
const FIX_2_562915447: i64 = 20_995;
const FIX_3_072711026: i64 = 25_172;

fn dequantize(coefficient: i16, quantization: u8) -> Result<i64> {
    i64::from(coefficient)
        .checked_mul(i64::from(quantization))
        .context("MVS mode 5 反量化乘法溢出")
}

fn descale(value: i64, shift: u32) -> i64 {
    (value + (1i64 << (shift - 1))) >> shift
}

fn apple_idct_range_limit(value: i64) -> u8 {
    let index = usize::try_from(value & 0x3ff).expect("10-bit mask always fits usize");
    match index {
        0..=127 => u8::try_from(index + 128).expect("range table high ramp fits u8"),
        128..=383 => 255,
        384..=895 => 0,
        896..=1023 => u8::try_from(index - 896).expect("range table low ramp fits u8"),
        _ => unreachable!("10-bit mask bounds the range-table index"),
    }
}

/// Apple `_jpeg_idct_islow` 的 13 位定点、两遍 8x8 逐字面转译。
fn inverse_dct_8x8(coefficients: &[i16; 64], quantization: &[u8; 64]) -> Result<[u8; 64]> {
    let mut workspace = [0i64; 64];

    for column in 0..8 {
        if (1..8).all(|row| coefficients[row * 8 + column] == 0) {
            let dc = dequantize(coefficients[column], quantization[column])?
                .checked_shl(IDCT_PASS1_BITS)
                .context("MVS mode 5 IDCT DC 列快捷路径溢出")?;
            for row in 0..8 {
                workspace[row * 8 + column] = dc;
            }
            continue;
        }

        let mut z2 = dequantize(coefficients[2 * 8 + column], quantization[2 * 8 + column])?;
        let mut z3 = dequantize(coefficients[6 * 8 + column], quantization[6 * 8 + column])?;
        let z1 = (z2 + z3) * FIX_0_541196100;
        let tmp2_even = z1 - z3 * FIX_1_847759065;
        let tmp3_even = z1 + z2 * FIX_0_765366865;

        z2 = dequantize(coefficients[column], quantization[column])?;
        z3 = dequantize(coefficients[4 * 8 + column], quantization[4 * 8 + column])?;
        let tmp0_even = (z2 + z3) << IDCT_CONST_BITS;
        let tmp1_even = (z2 - z3) << IDCT_CONST_BITS;
        let tmp10 = tmp0_even + tmp3_even;
        let tmp13 = tmp0_even - tmp3_even;
        let tmp11 = tmp1_even + tmp2_even;
        let tmp12 = tmp1_even - tmp2_even;

        let mut tmp0 = dequantize(coefficients[7 * 8 + column], quantization[7 * 8 + column])?;
        let mut tmp1 = dequantize(coefficients[5 * 8 + column], quantization[5 * 8 + column])?;
        let mut tmp2 = dequantize(coefficients[3 * 8 + column], quantization[3 * 8 + column])?;
        let mut tmp3 = dequantize(coefficients[8 + column], quantization[8 + column])?;
        let mut z1 = tmp0 + tmp3;
        let mut z2 = tmp1 + tmp2;
        let mut z3 = tmp0 + tmp2;
        let mut z4 = tmp1 + tmp3;
        let z5 = (z3 + z4) * FIX_1_175875602;

        tmp0 *= FIX_0_298631336;
        tmp1 *= FIX_2_053119869;
        tmp2 *= FIX_3_072711026;
        tmp3 *= FIX_1_501321110;
        z1 *= -FIX_0_899976223;
        z2 *= -FIX_2_562915447;
        z3 = z3 * -FIX_1_961570560 + z5;
        z4 = z4 * -FIX_0_390180644 + z5;
        tmp0 += z1 + z3;
        tmp1 += z2 + z4;
        tmp2 += z2 + z3;
        tmp3 += z1 + z4;

        workspace[column] = descale(tmp10 + tmp3, IDCT_CONST_BITS - IDCT_PASS1_BITS);
        workspace[7 * 8 + column] = descale(tmp10 - tmp3, IDCT_CONST_BITS - IDCT_PASS1_BITS);
        workspace[8 + column] = descale(tmp11 + tmp2, IDCT_CONST_BITS - IDCT_PASS1_BITS);
        workspace[6 * 8 + column] = descale(tmp11 - tmp2, IDCT_CONST_BITS - IDCT_PASS1_BITS);
        workspace[2 * 8 + column] = descale(tmp12 + tmp1, IDCT_CONST_BITS - IDCT_PASS1_BITS);
        workspace[5 * 8 + column] = descale(tmp12 - tmp1, IDCT_CONST_BITS - IDCT_PASS1_BITS);
        workspace[3 * 8 + column] = descale(tmp13 + tmp0, IDCT_CONST_BITS - IDCT_PASS1_BITS);
        workspace[4 * 8 + column] = descale(tmp13 - tmp0, IDCT_CONST_BITS - IDCT_PASS1_BITS);
    }

    let mut output = [0u8; 64];
    for row in 0..8 {
        let offset = row * 8;
        if workspace[offset + 1..offset + 8]
            .iter()
            .all(|&value| value == 0)
        {
            let sample = apple_idct_range_limit(descale(workspace[offset], IDCT_PASS1_BITS + 3));
            output[offset..offset + 8].fill(sample);
            continue;
        }

        let mut z2 = workspace[offset + 2];
        let mut z3 = workspace[offset + 6];
        let z1 = (z2 + z3) * FIX_0_541196100;
        let tmp2_even = z1 - z3 * FIX_1_847759065;
        let tmp3_even = z1 + z2 * FIX_0_765366865;
        z2 = workspace[offset];
        z3 = workspace[offset + 4];
        let tmp0_even = (z2 + z3) << IDCT_CONST_BITS;
        let tmp1_even = (z2 - z3) << IDCT_CONST_BITS;
        let tmp10 = tmp0_even + tmp3_even;
        let tmp13 = tmp0_even - tmp3_even;
        let tmp11 = tmp1_even + tmp2_even;
        let tmp12 = tmp1_even - tmp2_even;

        let mut tmp0 = workspace[offset + 7];
        let mut tmp1 = workspace[offset + 5];
        let mut tmp2 = workspace[offset + 3];
        let mut tmp3 = workspace[offset + 1];
        let mut z1 = tmp0 + tmp3;
        let mut z2 = tmp1 + tmp2;
        let mut z3 = tmp0 + tmp2;
        let mut z4 = tmp1 + tmp3;
        let z5 = (z3 + z4) * FIX_1_175875602;
        tmp0 *= FIX_0_298631336;
        tmp1 *= FIX_2_053119869;
        tmp2 *= FIX_3_072711026;
        tmp3 *= FIX_1_501321110;
        z1 *= -FIX_0_899976223;
        z2 *= -FIX_2_562915447;
        z3 = z3 * -FIX_1_961570560 + z5;
        z4 = z4 * -FIX_0_390180644 + z5;
        tmp0 += z1 + z3;
        tmp1 += z2 + z4;
        tmp2 += z2 + z3;
        tmp3 += z1 + z4;

        let shift = IDCT_CONST_BITS + IDCT_PASS1_BITS + 3;
        output[offset] = apple_idct_range_limit(descale(tmp10 + tmp3, shift));
        output[offset + 7] = apple_idct_range_limit(descale(tmp10 - tmp3, shift));
        output[offset + 1] = apple_idct_range_limit(descale(tmp11 + tmp2, shift));
        output[offset + 6] = apple_idct_range_limit(descale(tmp11 - tmp2, shift));
        output[offset + 2] = apple_idct_range_limit(descale(tmp12 + tmp1, shift));
        output[offset + 5] = apple_idct_range_limit(descale(tmp12 - tmp1, shift));
        output[offset + 3] = apple_idct_range_limit(descale(tmp13 + tmp0, shift));
        output[offset + 4] = apple_idct_range_limit(descale(tmp13 - tmp0, shift));
    }
    Ok(output)
}

/// Apple `_ycc_xrgb_convert20to32Pixel` 的窄化标量等价式。
///
/// Cb/Cr 在 wire 上各为 6 位，Apple 先左移两位，再用 IJG 的 16 位
/// 定点表（91881、116130、46802、22554）并做 8 位饱和。
fn ycbcr_20_to_rgb(y: u8, cb_six: u8, cr_six: u8) -> [u8; 3] {
    ycbcr_8_to_rgb(y, cb_six << 2, cr_six << 2)
}

fn ycbcr_8_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let y = i32::from(y);
    let cb = i32::from(cb) - 128;
    let cr = i32::from(cr) - 128;
    let red = y + ((91_881 * cr + 32_768) >> 16);
    let blue = y + ((116_130 * cb + 32_768) >> 16);
    let green = y + ((-46_802 * cr - 22_554 * cb + 32_768) >> 16);
    [
        clamp_channel(red),
        clamp_channel(green),
        clamp_channel(blue),
    ]
}

fn clamp_channel(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

fn pixel_offset(width: usize, x: usize, y: usize) -> usize {
    (y * width + x) * RGB_CHANNELS
}

fn write_pixel(rgb: &mut [u8], width: usize, x: usize, y: usize, color: [u8; 3]) {
    let offset = pixel_offset(width, x, y);
    rgb[offset..offset + RGB_CHANNELS].copy_from_slice(&color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vnc::mvs::MAX_MVS_DECODE_PIXELS;
    use crate::vnc::mvs_wire::{MvsFullRecord, MvsTables};

    struct TestBitWriter {
        bytes: Vec<u8>,
        bit_len: usize,
    }

    impl TestBitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                bit_len: 0,
            }
        }

        fn write_bits(&mut self, value: u32, count: u8) {
            assert!(count <= u32::BITS as u8);
            for shift in (0..count).rev() {
                if self.bit_len % 8 == 0 {
                    self.bytes.push(0);
                }
                let bit = ((value >> shift) & 1) as u8;
                let byte = self.bit_len / 8;
                self.bytes[byte] |= bit << (7 - self.bit_len % 8);
                self.bit_len += 1;
            }
        }

        fn write_bit_string(&mut self, bits: &str) {
            for bit in bits.bytes() {
                match bit {
                    b'0' => self.write_bits(0, 1),
                    b'1' => self.write_bits(1, 1),
                    b' ' | b'_' => {}
                    _ => panic!("invalid bit-string character"),
                }
            }
        }

        fn finish(self) -> Vec<u8> {
            self.bytes
        }
    }

    fn write_repeat(writer: &mut TestBitWriter, repeat: usize) {
        if repeat == 0 {
            writer.write_bits(0, 1);
        } else {
            assert!(repeat <= 15);
            writer.write_bits(1, 1);
            writer.write_bits(u32::try_from(repeat - 1).unwrap(), 4);
        }
    }

    fn mode_stream(runs: &[(u8, usize)]) -> Vec<u8> {
        let mut writer = TestBitWriter::new();
        writer.write_bits(0, 1); // Apple consumes one leading mode-stream bit.
        for &(mode, repeat) in runs {
            writer.write_bits(u32::from(mode), 3);
            write_repeat(&mut writer, repeat);
        }
        writer.write_bits(0x6d, 8);
        writer.finish()
    }

    fn terminal_only_data() -> Vec<u8> {
        let mut writer = TestBitWriter::new();
        writer.write_bits(0x6d, 8);
        writer.finish()
    }

    fn two_asymmetric_mode_three_tiles_data() -> Vec<u8> {
        let mut writer = TestBitWriter::new();
        for rows in [
            [0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01],
            [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80],
        ] {
            writer.write_bits(0, 8); // Each row carries its own pixel mask.
            for row in rows {
                writer.write_bits(row, 8);
            }
        }
        writer.write_bits(0x6d, 8);
        writer.finish()
    }

    fn record<'a>(mode_stream: &'a [u8], data_stream: &'a [u8]) -> MvsFullRecord<'a> {
        record_with_thresholds(mode_stream, data_stream, 0, 0)
    }

    fn record_with_thresholds<'a>(
        mode_stream: &'a [u8],
        data_stream: &'a [u8],
        scale_threshold_a: u8,
        scale_threshold_b: u8,
    ) -> MvsFullRecord<'a> {
        MvsFullRecord {
            scale_threshold_a,
            scale_threshold_b,
            mode_stream,
            data_stream,
        }
    }

    fn decoder() -> MvsFullDecoder {
        MvsFullDecoder::new(MvsTables {
            luminance: [0; 64],
            chrominance: [0; 64],
        })
    }

    fn decoder_with_tables(luminance: u8, chrominance: u8) -> MvsFullDecoder {
        MvsFullDecoder::new(MvsTables {
            luminance: [luminance; 64],
            chrominance: [chrominance; 64],
        })
    }

    fn mode_five_data(tile_literals: &[&str]) -> Vec<u8> {
        let mut writer = TestBitWriter::new();
        for literal in tile_literals {
            writer.write_bit_string(literal);
        }
        writer.write_bits(0x6d, 8);
        writer.finish()
    }

    fn asymmetric_seed_mode_five_data(tile_count: usize) -> Vec<u8> {
        let cb_minus_forty = "11111110_000_1";
        let cr_plus_twenty_four = "111110_000_0";
        let y_minus_eighty = "1111111111110_000_1";
        let literal = format!("0_0_0_{cb_minus_forty}_{cr_plus_twenty_four}_{y_minus_eighty}_0010");
        let literals = vec![literal.as_str(); tile_count];
        mode_five_data(&literals)
    }

    fn mode_three_zero_bitmap_data() -> Vec<u8> {
        let mut writer = TestBitWriter::new();
        writer.write_bits(0xff, 8);
        writer.write_bits(STREAM_TERMINAL as u32, 8);
        writer.finish()
    }

    fn mode_four_reused_single_color_data() -> Vec<u8> {
        let mut writer = TestBitWriter::new();
        writer.write_bits(0, 1);
        writer.write_bits(1, 1);
        writer.write_bits(STREAM_TERMINAL as u32, 8);
        writer.finish()
    }

    fn coefficient_seed_snapshot(
        decoder: &MvsFullDecoder,
        tile_index: usize,
    ) -> Option<(u8, [i8; 64], u8, i8, u8, i8)> {
        let seed = decoder
            .surface_state
            .as_ref()?
            .tiles
            .get(tile_index)?
            .coefficients
            .as_ref()?;
        Some((
            seed.selected_threshold,
            seed.y,
            seed.cb_count,
            seed.cb_dc,
            seed.cr_count,
            seed.cr_dc,
        ))
    }

    fn reference_snapshot(decoder: &MvsFullDecoder, tile_index: usize) -> Option<(usize, usize)> {
        let reference = decoder
            .surface_state
            .as_ref()?
            .tiles
            .get(tile_index)?
            .reference
            .as_ref()?;
        Some((reference.tile_index, reference.framebuffer_offset))
    }

    fn partial_payload(chroma_extent: u8, cr_extent: u8, fields: &[&str]) -> Vec<u8> {
        partial_payload_with_terminals(chroma_extent, cr_extent, fields, [0x6d, 0x76, 0x73])
    }

    fn partial_payload_with_terminals(
        chroma_extent: u8,
        cr_extent: u8,
        fields: &[&str],
        terminals: [u8; 3],
    ) -> Vec<u8> {
        let mut writer = TestBitWriter::new();
        for field in fields {
            writer.write_bit_string(field);
        }
        for terminal in terminals {
            writer.write_bits(u32::from(terminal), 8);
        }
        let mut payload = vec![1, chroma_extent, cr_extent];
        payload.extend(writer.finish());
        payload
    }

    fn seed_asymmetric_surface(decoder: &mut MvsFullDecoder, width: usize, height: usize) {
        let tile_count = width.div_ceil(TILE_EDGE) * height.div_ceil(TILE_EDGE);
        let modes = mode_stream(&[(5, tile_count - 1)]);
        let data = asymmetric_seed_mode_five_data(tile_count);
        let record = record_with_thresholds(&modes, &data, 5, 31);
        let prepared = decoder.prepare(&record, width, height).unwrap();
        decoder.commit(prepared);
    }

    fn cache_entry_snapshot(decoder: &MvsFullDecoder, index: usize) -> Option<[i8; 99]> {
        decoder
            .cache_state
            .entries
            .get(index)
            .and_then(Option::as_ref)
            .map(|entry| **entry)
    }

    fn cache_index_snapshot(decoder: &MvsFullDecoder) -> (u16, u16, u32) {
        (
            decoder.cache_state.previous_cache_index,
            decoder.cache_state.last_insert_index,
            decoder.cache_state.population_count,
        )
    }

    fn ac_cache_entry(horizontal_direction: i8) -> [i8; MVS_CACHE_ENTRY_BYTES] {
        let mut entry = [0i8; MVS_CACHE_ENTRY_BYTES];
        // Hand cache fixture: signed scan-1 coefficient +/-2 with luminance
        // quantization 64 matches the independently frozen native IDCT vector.
        entry[1] = horizontal_direction.checked_mul(2).unwrap();
        entry
    }

    fn install_cache_entry(
        decoder: &mut MvsFullDecoder,
        index: u16,
        entry: [i8; MVS_CACHE_ENTRY_BYTES],
        population_count: u32,
    ) {
        Arc::make_mut(&mut decoder.cache_state.entries)[usize::from(index)] = Some(Arc::new(entry));
        decoder.cache_state.population_count = population_count;
    }

    fn cache_mode_data(indices: &[u16]) -> Vec<u8> {
        let mut data = Vec::with_capacity(indices.len() * 2 + 1);
        for index in indices {
            data.extend_from_slice(&index.to_be_bytes());
        }
        data.push(STREAM_TERMINAL);
        data
    }

    fn pixel(decoded: &DecodedMvsRect, x: usize, y: usize) -> [u8; 3] {
        let offset = (y * decoded.width + x) * 3;
        decoded.rgb[offset..offset + 3].try_into().unwrap()
    }

    fn complete_tile(decoded: &DecodedMvsRect, tile_x: usize, tile_y: usize) -> Vec<u8> {
        let mut tile = Vec::with_capacity(8 * 8 * 3);
        for y in tile_y..tile_y + 8 {
            let start = (y * decoded.width + tile_x) * 3;
            tile.extend_from_slice(&decoded.rgb[start..start + 8 * 3]);
        }
        tile
    }

    fn prepare(
        decoder: &MvsFullDecoder,
        modes: &[u8],
        data: &[u8],
        width: usize,
        height: usize,
    ) -> anyhow::Result<PreparedMvsFull> {
        decoder.prepare(&record(modes, data), width, height)
    }

    fn exact_reader_bytes(bits: &str) -> (Vec<u8>, u8) {
        let bit_count = bits
            .bytes()
            .filter(|bit| matches!(bit, b'0' | b'1'))
            .count();
        let prefix = u8::try_from((8 - bit_count % 8) % 8).unwrap();
        let mut writer = TestBitWriter::new();
        writer.write_bits(0, prefix);
        writer.write_bit_string(bits);
        (writer.finish(), prefix)
    }

    fn decode_dc_bits(bits: &str) -> anyhow::Result<i16> {
        let (bytes, prefix) = exact_reader_bytes(bits);
        let mut reader = BitReader::new(&bytes);
        reader.read_bits(prefix)?;
        decode_dc_rice(&mut reader)
    }

    fn decode_ac_bits(bits: &str, threshold: u8) -> anyhow::Result<[i16; 64]> {
        let (bytes, prefix) = exact_reader_bytes(bits);
        let mut reader = BitReader::new(&bytes);
        reader.read_bits(prefix)?;
        decode_luma_ac_rice(&mut reader, threshold)
    }

    #[test]
    fn dc_rice_literals_lock_signed_mapping_and_magnitude_boundaries() {
        for (bits, expected) in [
            ("00", 0),
            ("010", 1),
            ("011", -1),
            ("1000", 2),
            ("1011", -3),
            ("110000", 4),
            ("110111", -7),
            ("11100000", 8),
            ("11101111", -15),
        ] {
            assert_eq!(decode_dc_bits(bits).unwrap(), expected, "bits={bits}");
        }
    }

    #[test]
    fn dc_rice_accepts_q38_and_rejects_the_fixed_q39_guard() {
        let accepted = format!("{}0_101_0", "1".repeat(38));
        assert_eq!(decode_dc_bits(&accepted).unwrap(), 293);

        let rejected = format!("{}0_000_0", "1".repeat(39));
        assert!(decode_dc_bits(&rejected).is_err());
    }

    #[test]
    fn ac_special_tokens_lock_zero_eob_and_minimum_sign_mapping() {
        assert_eq!(decode_ac_bits("0010", 0).unwrap(), [0; 64]);

        let mut positive = [0i16; 64];
        positive[1] = 16;
        assert_eq!(decode_ac_bits("010_0010", 0).unwrap(), positive);

        let mut negative = [0i16; 64];
        negative[1] = -16;
        assert_eq!(decode_ac_bits("011_0010", 0).unwrap(), negative);
    }

    #[test]
    fn ac_magnitude_literals_lock_both_band_formulas_and_signs() {
        let mut first_q3 = [0i16; 64];
        first_q3[1] = 17 << 4;
        assert_eq!(decode_ac_bits("11110_11_0_0010", 0).unwrap(), first_q3);

        let mut first_q4 = [0i16; 64];
        first_q4[1] = -(18 << 4);
        assert_eq!(decode_ac_bits("111110_000_1_0010", 0).unwrap(), first_q4);

        let first_five_zeros = "000".repeat(5);
        let mut second_q0 = [0i16; 64];
        second_q0[3] = 3 << 4;
        assert_eq!(
            decode_ac_bits(&format!("{first_five_zeros}10_1_0_0010"), 0).unwrap(),
            second_q0
        );

        let mut second_q1 = [0i16; 64];
        second_q1[3] = -(4 << 4);
        assert_eq!(
            decode_ac_bits(&format!("{first_five_zeros}110_00_1_0010"), 0).unwrap(),
            second_q1
        );

        let mut second_q2 = [0i16; 64];
        second_q2[3] = 8 << 4;
        assert_eq!(
            decode_ac_bits(&format!("{first_five_zeros}1110_000_0_0010"), 0).unwrap(),
            second_q2
        );
    }

    #[test]
    fn ac_fixed_unary_guards_preserve_first_band_error_and_second_band_fail_soft() {
        let first_accepted = format!("1{}0_101_0_0010", "1".repeat(32));
        assert!(decode_ac_bits(&first_accepted, 0).is_ok());
        let first_rejected = format!("1{}0_000_0", "1".repeat(33));
        assert!(decode_ac_bits(&first_rejected, 0).is_err());

        let lead = "000".repeat(5);
        let second_accepted = format!("{lead}1{}0_101_0_0010", "1".repeat(18));
        assert!(decode_ac_bits(&second_accepted, 0).is_ok());
        let second_fail_soft = format!("{lead}1{}", "1".repeat(19));
        assert!(decode_ac_bits(&second_fail_soft, 0).is_ok());
    }

    #[test]
    fn second_band_q19_fail_soft_preserves_partial_coefficients_and_cursor() {
        let mut writer = TestBitWriter::new();
        writer.write_bit_string("010");
        writer.write_bit_string(&"000".repeat(4));
        writer.write_bit_string("1");
        writer.write_bit_string(&"1".repeat(19));
        writer.write_bits(STREAM_TERMINAL as u32, 8);
        let bytes = writer.finish();
        let mut reader = BitReader::new(&bytes);

        let mut expected = [0i16; 64];
        expected[1] = 2;
        assert_eq!(decode_luma_ac_rice(&mut reader, 15).unwrap(), expected);
        assert_eq!(reader.bit_position(), 35);
        assert_eq!(reader.read_u8().unwrap(), STREAM_TERMINAL);
    }

    #[test]
    fn apple_natural_order_is_the_complete_explicit_sixty_four_entry_map() {
        assert_eq!(
            APPLE_NATURAL_ORDER,
            [
                0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41,
                34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23,
                30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
            ]
        );
    }

    #[test]
    fn extended_zero_run_lands_exactly_at_coefficient_sixty_three() {
        let mut expected = [0i16; 64];
        expected[63] = 16;
        assert_eq!(
            decode_ac_bits("0_01_1_11_111_111_111_111_111_111_111_111_000_010", 0,).unwrap(),
            expected
        );
    }

    #[test]
    fn short_zero_run_past_sixty_three_finishes_without_an_implicit_increment() {
        let lead = "000".repeat(59);
        assert_eq!(
            decode_ac_bits(&format!("{lead}0_01_1_10"), 0).unwrap(),
            [0; 64]
        );
    }

    #[test]
    fn rice_truncation_is_rejected_at_unary_remainder_token_run_and_level_fields() {
        assert!(decode_dc_bits("").is_err());
        assert!(decode_dc_bits("01").is_err());
        assert!(decode_dc_bits("10").is_err());

        assert!(decode_ac_bits("0", 0).is_err());
        assert!(decode_ac_bits("001", 0).is_err());
        assert!(decode_ac_bits("0011", 0).is_err());
        assert!(decode_ac_bits("001111", 0).is_err());
        assert!(decode_ac_bits("10", 0).is_err());
    }

    #[test]
    fn scale_threshold_selects_the_recovered_index_dependent_shifts() {
        assert_eq!(luma_ac_shift(1, 0).unwrap(), 4);
        assert_eq!(luma_ac_shift(1, 5).unwrap(), 3);
        assert_eq!(luma_ac_shift(5, 5).unwrap(), 4);
        assert_eq!(luma_ac_shift(6, 5).unwrap(), 4);
        assert_eq!(luma_ac_shift(1, 15).unwrap(), 1);
        assert_eq!(luma_ac_shift(6, 15).unwrap(), 1);
        assert_eq!(luma_ac_shift(15, 15).unwrap(), 3);
        assert!(luma_ac_shift(0, 0).is_err());
        assert!(luma_ac_shift(64, 0).is_err());
    }

    #[test]
    fn idct_all_zero_and_signed_dc_only_blocks_are_uniform() {
        let quantization = [1u8; 64];
        assert_eq!(inverse_dct_8x8(&[0; 64], &quantization).unwrap(), [128; 64]);

        let mut positive = [0i16; 64];
        positive[0] = 80;
        assert_eq!(
            inverse_dct_8x8(&positive, &quantization).unwrap(),
            [138; 64]
        );

        let mut negative = [0i16; 64];
        negative[0] = -80;
        assert_eq!(
            inverse_dct_8x8(&negative, &quantization).unwrap(),
            [118; 64]
        );
    }

    #[test]
    fn apple_ten_bit_masked_range_table_locks_all_piecewise_boundaries() {
        assert_eq!(
            [
                apple_idct_range_limit(383),
                apple_idct_range_limit(384),
                apple_idct_range_limit(895),
                apple_idct_range_limit(896),
                apple_idct_range_limit(1023),
                apple_idct_range_limit(1024),
                apple_idct_range_limit(-1024),
            ],
            [255, 0, 0, 0, 127, 128, 128]
        );
    }

    #[test]
    fn idct_horizontal_ac_impulse_locks_orientation_and_both_passes() {
        let mut coefficients = [0i16; 64];
        coefficients[1] = 64;
        let expected_row = [139, 137, 134, 130, 126, 122, 119, 117];
        let expected = [
            139, 137, 134, 130, 126, 122, 119, 117, 139, 137, 134, 130, 126, 122, 119, 117, 139,
            137, 134, 130, 126, 122, 119, 117, 139, 137, 134, 130, 126, 122, 119, 117, 139, 137,
            134, 130, 126, 122, 119, 117, 139, 137, 134, 130, 126, 122, 119, 117, 139, 137, 134,
            130, 126, 122, 119, 117, 139, 137, 134, 130, 126, 122, 119, 117,
        ];
        assert_eq!(expected[..8], expected_row);
        assert_eq!(inverse_dct_8x8(&coefficients, &[1; 64]).unwrap(), expected);
    }

    #[test]
    fn idct_mixed_literal_exercises_column_and_row_rounding() {
        let mut coefficients = [0i16; 64];
        coefficients[0] = 32;
        coefficients[1] = 64;
        coefficients[8] = -32;
        coefficients[19] = 24;
        assert_eq!(
            inverse_dct_8x8(&coefficients, &[1; 64]).unwrap(),
            [
                142, 135, 127, 126, 127, 126, 118, 111, 140, 136, 131, 128, 126, 123, 118, 114,
                138, 139, 137, 132, 125, 120, 119, 120, 137, 141, 143, 136, 126, 119, 120, 124,
                140, 144, 145, 138, 128, 121, 123, 127, 144, 145, 144, 139, 132, 127, 125, 126,
                150, 146, 141, 138, 136, 133, 128, 124, 153, 146, 138, 137, 138, 137, 129, 122,
            ]
        );
    }

    #[test]
    fn idct_saturates_large_signed_dc_outputs() {
        let mut positive = [0i16; 64];
        positive[0] = 2048;
        assert_eq!(inverse_dct_8x8(&positive, &[1; 64]).unwrap(), [255; 64]);

        let mut negative = [0i16; 64];
        negative[0] = -2048;
        assert_eq!(inverse_dct_8x8(&negative, &[1; 64]).unwrap(), [0; 64]);
    }

    #[test]
    fn quantization_multiply_preserves_signed_extremes_without_i16_wrap() {
        assert_eq!(dequantize(i16::MAX, u8::MAX).unwrap(), 8_355_585);
        assert_eq!(dequantize(i16::MIN, u8::MAX).unwrap(), -8_355_840);
    }

    #[test]
    fn shared_eight_bit_color_converter_preserves_neutral_and_component_order() {
        assert_eq!(ycbcr_8_to_rgb(100, 128, 128), [100, 100, 100]);
        assert_eq!(ycbcr_8_to_rgb(100, 0, 255), [255, 53, 0]);
        assert_eq!(ycbcr_8_to_rgb(100, 255, 0), [0, 148, 255]);
        assert_eq!(ycbcr_20_to_rgb(100, 16, 48), [190, 76, 0]);
    }

    #[test]
    fn mode_five_dc_only_tile_decodes_complete_dual_stream_to_neutral_rgb() {
        let modes = mode_stream(&[(5, 0)]);
        let data = mode_five_data(&["0_1_0_00_0010"]);
        let prepared = prepare(&decoder_with_tables(1, 1), &modes, &data, 8, 8).unwrap();
        assert_eq!(MvsFullDecoder::decoded(&prepared).rgb, vec![128; 8 * 8 * 3]);
    }

    #[test]
    fn adjacent_mode_five_tiles_apply_previous_dc_predictor_in_decode_order() {
        let modes = mode_stream(&[(5, 1)]);
        let first_y_minus_eighty = "1111111111110_000_1";
        let second_y_minus_eight = "1110_000_1";
        let data = mode_five_data(&[
            &format!("0_0_0_00_00_{first_y_minus_eighty}_0010"),
            &format!("0_1_0_{second_y_minus_eight}_0010"),
        ]);
        let prepared = prepare(&decoder_with_tables(1, 1), &modes, &data, 16, 8).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        assert_eq!(pixel(decoded, 0, 0), [138; 3]);
        assert_eq!(pixel(decoded, 7, 7), [138; 3]);
        assert_eq!(pixel(decoded, 8, 0), [139; 3]);
        assert_eq!(pixel(decoded, 15, 7), [139; 3]);
    }

    #[test]
    fn adjacent_mode_five_tile_reuses_asymmetric_nonzero_chroma_predictors() {
        let modes = mode_stream(&[(5, 1)]);
        let cb_minus_forty = "11111110_000_1";
        let cr_plus_twenty_four = "111110_000_0";
        let y_minus_eight = "1110_000_1";
        let data = mode_five_data(&[
            &format!("0_0_0_{cb_minus_forty}_{cr_plus_twenty_four}_00_0010"),
            &format!("0_1_0_{y_minus_eight}_0010"),
        ]);
        let prepared = prepare(&decoder_with_tables(1, 1), &modes, &data, 16, 8).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);

        assert!(complete_tile(decoded, 0, 0)
            .chunks_exact(3)
            .all(|rgb| rgb == [120, 129, 146]));
        assert!(complete_tile(decoded, 8, 0)
            .chunks_exact(3)
            .all(|rgb| rgb == [121, 130, 147]));
    }

    #[test]
    fn mode_five_asymmetric_ac_tile_preserves_full_record_orientation() {
        let modes = mode_stream(&[(5, 0)]);
        let data = mode_five_data(&["0_1_0_00_010_0010"]);
        let record = record_with_thresholds(&modes, &data, 15, 0);
        let decoder = decoder_with_tables(64, 1);
        let prepared = decoder.prepare(&record, 8, 8).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        let expected = [150, 147, 141, 132, 124, 115, 109, 106];
        for y in 0..8 {
            for (x, &channel) in expected.iter().enumerate() {
                assert_eq!(pixel(decoded, x, y), [channel; 3]);
            }
        }
    }

    #[test]
    fn nonuniform_quantization_uses_nonzero_natural_ac_index_in_pixels() {
        let modes = mode_stream(&[(5, 0)]);
        let data = mode_five_data(&["0_1_0_00_010_0010"]);
        let record = record_with_thresholds(&modes, &data, 15, 0);
        let mut luminance = [0u8; 64];
        luminance[1] = 64;
        let decoder = MvsFullDecoder::new(MvsTables {
            luminance,
            chrominance: [1; 64],
        });
        let prepared = decoder.prepare(&record, 8, 8).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        let expected = [
            [150, 150, 150],
            [147, 147, 147],
            [141, 141, 141],
            [132, 132, 132],
            [124, 124, 124],
            [115, 115, 115],
            [109, 109, 109],
            [106, 106, 106],
        ];
        for y in 0..8 {
            for (x, &expected_pixel) in expected.iter().enumerate() {
                assert_eq!(pixel(decoded, x, y), expected_pixel);
            }
        }
    }

    #[test]
    fn second_band_q19_fail_soft_publishes_partial_coefficients_for_copy() {
        let modes = mode_stream(&[(5, 1)]);
        let q19 = format!("0_1_0_00_010_{}1{}", "000".repeat(4), "1".repeat(19));
        let data = mode_five_data(&[&q19, "1"]);
        let record = record_with_thresholds(&modes, &data, 15, 0);
        let prepared = decoder_with_tables(64, 1).prepare(&record, 16, 8).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);

        assert_eq!(complete_tile(decoded, 0, 0), complete_tile(decoded, 8, 0));
        assert_eq!(pixel(decoded, 0, 0), [150; 3]);
        assert_eq!(pixel(decoded, 7, 7), [106; 3]);
    }

    #[test]
    fn mode_five_tile_header_selects_scale_threshold_b_only_when_set() {
        let modes = mode_stream(&[(5, 1)]);
        let data = mode_five_data(&["0_1_0_00_010_0010", "0_1_1_00_010_0010"]);
        let record = record_with_thresholds(&modes, &data, 0, 15);
        let decoder = decoder_with_tables(8, 1);
        let prepared = decoder.prepare(&record, 16, 8).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        assert_eq!(pixel(decoded, 0, 0), [150; 3]);
        assert_eq!(pixel(decoded, 8, 0), [131; 3]);
    }

    #[test]
    fn mode_five_clipped_ten_by_nine_consumes_all_four_logical_tiles() {
        let modes = mode_stream(&[(5, 3)]);
        let data = mode_five_data(&[
            "0_1_0_00_0010",
            "0_1_0_00_0010",
            "0_1_0_00_0010",
            "0_1_0_00_0010",
        ]);
        let prepared = prepare(&decoder_with_tables(1, 1), &modes, &data, 10, 9).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        assert_eq!(decoded.rgb.len(), 10 * 9 * 3);
        assert!(decoded.rgb.iter().all(|&channel| channel == 128));
    }

    #[test]
    fn distinct_luma_and_chroma_tables_produce_literal_mode_five_rgb() {
        let modes = mode_stream(&[(5, 0)]);
        let minus_forty = "11111110_000_1";
        let data = mode_five_data(&[&format!("0_0_0_{minus_forty}_00_{minus_forty}_0010")]);
        let prepared = prepare(&decoder_with_tables(2, 1), &modes, &data, 8, 8).unwrap();
        assert!(MvsFullDecoder::decoded(&prepared)
            .rgb
            .chunks_exact(3)
            .all(|rgb| rgb == [138, 135, 156]));
    }

    #[test]
    fn malformed_mode_five_ac_after_valid_components_commits_neither_pixels_nor_state() {
        let modes = mode_stream(&[(5, 0)]);
        let malformed = mode_five_data(&[&format!("0_0_0_00_00_00_1{}", "1".repeat(33))]);
        let valid = mode_five_data(&["0_1_0_00_0010"]);
        let decoder = decoder_with_tables(1, 1);
        assert!(prepare(&decoder, &modes, &malformed, 8, 8).is_err());
        assert_eq!(decoder.committed_records, 0);

        let after_failure = prepare(&decoder, &modes, &valid, 8, 8).unwrap();
        let fresh = prepare(&decoder_with_tables(1, 1), &modes, &valid, 8, 8).unwrap();
        assert_eq!(
            MvsFullDecoder::decoded(&after_failure),
            MvsFullDecoder::decoded(&fresh)
        );
    }

    #[test]
    fn dropped_or_committed_mode_five_prepare_does_not_leak_record_local_predictors() {
        let modes = mode_stream(&[(5, 0)]);
        let y_minus_eighty = "1111111111110_000_1";
        let data = mode_five_data(&[&format!("0_0_0_00_00_{y_minus_eighty}_0010")]);
        let mut decoder = decoder_with_tables(1, 1);

        let dropped = prepare(&decoder, &modes, &data, 8, 8).unwrap();
        assert_eq!(pixel(MvsFullDecoder::decoded(&dropped), 0, 0), [138; 3]);
        drop(dropped);
        assert_eq!(decoder.committed_records, 0);

        let committed = prepare(&decoder, &modes, &data, 8, 8).unwrap();
        decoder.commit(committed);
        assert_eq!(decoder.committed_records, 1);

        let next_record = prepare(&decoder, &modes, &data, 8, 8).unwrap();
        let fresh = prepare(&decoder_with_tables(1, 1), &modes, &data, 8, 8).unwrap();
        assert_eq!(
            MvsFullDecoder::decoded(&next_record),
            MvsFullDecoder::decoded(&fresh)
        );
    }

    #[test]
    fn mode_five_copy_uses_only_the_immediately_previous_mode_five_tile() {
        let modes = mode_stream(&[(5, 1)]);
        let y_minus_eighty = "1111111111110_000_1";
        let data = mode_five_data(&[&format!("0_0_0_00_00_{y_minus_eighty}_0010"), "1"]);
        let prepared = prepare(&decoder_with_tables(1, 1), &modes, &data, 16, 8).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        assert_eq!(complete_tile(decoded, 0, 0), complete_tile(decoded, 8, 0));

        let invalid = mode_five_data(&["1"]);
        let cleared = prepare(
            &decoder_with_tables(1, 1),
            &mode_stream(&[(5, 0)]),
            &invalid,
            8,
            8,
        )
        .unwrap();
        assert_eq!(MvsFullDecoder::decoded(&cleared).rgb, vec![128; 8 * 8 * 3]);
    }

    #[test]
    fn mode_five_copy_preserves_the_complete_ac_bearing_source_tile() {
        let modes = mode_stream(&[(5, 1)]);
        let cb_minus_forty = "11111110_000_1";
        let cr_plus_twenty_four = "111110_000_0";
        let source = format!("0_0_0_{cb_minus_forty}_{cr_plus_twenty_four}_00_010_0010");
        let data = mode_five_data(&[&source, "1"]);
        let record = record_with_thresholds(&modes, &data, 15, 0);
        let prepared = decoder_with_tables(64, 1).prepare(&record, 16, 8).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        let source_tile = complete_tile(decoded, 0, 0);
        let copied_tile = complete_tile(decoded, 8, 0);

        assert_eq!(pixel(decoded, 0, 0), [142, 151, 168]);
        assert_eq!(pixel(decoded, 7, 0), [98, 107, 124]);
        assert_ne!(pixel(decoded, 0, 0), pixel(decoded, 7, 0));
        assert_eq!(copied_tile, source_tile);
    }

    #[test]
    fn slice_a_mode_five_seed_is_staged_and_commit_serializes_exact_signed_bytes() {
        let modes = mode_stream(&[(5, 0)]);
        let data = asymmetric_seed_mode_five_data(1);
        let record = record_with_thresholds(&modes, &data, 15, 31);
        let mut decoder = decoder_with_tables(1, 1);

        let prepared = decoder.prepare(&record, 8, 8).unwrap();
        assert_eq!(coefficient_seed_snapshot(&decoder, 0), None);
        decoder.commit(prepared);

        let mut expected_y = [0i8; 64];
        expected_y[0] = 80;
        assert_eq!(
            coefficient_seed_snapshot(&decoder, 0),
            Some((15, expected_y, 1, 80, 1, -48))
        );
    }

    #[test]
    fn slice_a_mode_five_copy_clones_the_asymmetric_predecessor_seed_exactly() {
        let modes = mode_stream(&[(5, 1)]);
        let cb_minus_forty = "11111110_000_1";
        let cr_plus_twenty_four = "111110_000_0";
        let y_minus_eighty = "1111111111110_000_1";
        let source = format!("0_0_0_{cb_minus_forty}_{cr_plus_twenty_four}_{y_minus_eighty}_0010");
        let data = mode_five_data(&[&source, "1"]);
        let record = record_with_thresholds(&modes, &data, 15, 31);
        let mut decoder = decoder_with_tables(1, 1);

        let prepared = decoder.prepare(&record, 16, 8).unwrap();
        decoder.commit(prepared);

        assert_eq!(
            coefficient_seed_snapshot(&decoder, 1),
            coefficient_seed_snapshot(&decoder, 0)
        );
    }

    #[test]
    fn slice_a_dropped_seed_and_no_source_copy_preserve_exact_committed_presence() {
        let seed_modes = mode_stream(&[(5, 0)]);
        let seed_data = asymmetric_seed_mode_five_data(1);
        let seed_record = record_with_thresholds(&seed_modes, &seed_data, 15, 0);
        let copy_modes = mode_stream(&[(5, 0)]);
        let copy_data = mode_five_data(&["1"]);
        let copy_record = record_with_thresholds(&copy_modes, &copy_data, 0, 0);
        let mut decoder = decoder_with_tables(1, 1);

        drop(decoder.prepare(&seed_record, 8, 8).unwrap());
        assert_eq!(coefficient_seed_snapshot(&decoder, 0), None);

        let seeded = decoder.prepare(&seed_record, 8, 8).unwrap();
        decoder.commit(seeded);
        let committed = coefficient_seed_snapshot(&decoder, 0).unwrap();
        let copied = decoder.prepare(&copy_record, 8, 8).unwrap();
        decoder.commit(copied);
        assert_eq!(coefficient_seed_snapshot(&decoder, 0), Some(committed));

        let mut fresh = decoder_with_tables(1, 1);
        let absent = fresh.prepare(&copy_record, 8, 8).unwrap();
        fresh.commit(absent);
        assert_eq!(coefficient_seed_snapshot(&fresh, 0), None);
    }

    #[test]
    fn slice_a_modes_zero_three_and_four_retain_but_never_create_coefficient_seed() {
        let seed_modes = mode_stream(&[(5, 0)]);
        let seed_data = asymmetric_seed_mode_five_data(1);
        let seed_record = record_with_thresholds(&seed_modes, &seed_data, 15, 0);

        for (mode, data) in [
            (0, terminal_only_data()),
            (3, mode_three_zero_bitmap_data()),
            (4, mode_four_reused_single_color_data()),
        ] {
            let modes = mode_stream(&[(mode, 0)]);
            let record = record(&modes, &data);
            let mut seeded = decoder_with_tables(1, 1);
            let prepared = seeded.prepare(&seed_record, 8, 8).unwrap();
            seeded.commit(prepared);
            let before = coefficient_seed_snapshot(&seeded, 0).unwrap();
            let prepared = seeded.prepare(&record, 8, 8).unwrap();
            seeded.commit(prepared);
            assert_eq!(coefficient_seed_snapshot(&seeded, 0), Some(before));

            let mut fresh = decoder_with_tables(1, 1);
            let prepared = fresh.prepare(&record, 8, 8).unwrap();
            fresh.commit(prepared);
            assert_eq!(coefficient_seed_snapshot(&fresh, 0), None);
        }
    }

    #[test]
    fn slice_a_modes_one_and_two_replace_reference_metadata_without_clearing_seed() {
        for (width, height, copy_mode) in [(16, 8, 1), (8, 16, 2)] {
            let seed_modes = mode_stream(&[(5, 1)]);
            let seed_data = asymmetric_seed_mode_five_data(2);
            let seed_record = record_with_thresholds(&seed_modes, &seed_data, 15, 0);
            let copy_modes = mode_stream(&[(0, 0), (copy_mode, 0)]);
            let copy_data = terminal_only_data();
            let copy_record = record(&copy_modes, &copy_data);
            let mut decoder = decoder_with_tables(1, 1);

            let prepared = decoder.prepare(&seed_record, width, height).unwrap();
            decoder.commit(prepared);
            let before = coefficient_seed_snapshot(&decoder, 1).unwrap();
            let prepared = decoder.prepare(&copy_record, width, height).unwrap();
            decoder.commit(prepared);

            assert_eq!(coefficient_seed_snapshot(&decoder, 1), Some(before));
            assert_eq!(reference_snapshot(&decoder, 1), Some((0, 0)));
        }
    }

    #[test]
    fn slice_a_subrectangle_updates_only_its_global_seed_metadata() {
        let seed_modes = mode_stream(&[(5, 1)]);
        let seed_data = asymmetric_seed_mode_five_data(2);
        let seed_record = record_with_thresholds(&seed_modes, &seed_data, 15, 0);
        let mut decoder = decoder_with_tables(1, 1);
        let prepared = decoder.prepare(&seed_record, 16, 8).unwrap();
        decoder.commit(prepared);
        let first = coefficient_seed_snapshot(&decoder, 0).unwrap();
        let second = coefficient_seed_snapshot(&decoder, 1).unwrap();

        let sub_modes = mode_stream(&[(0, 0)]);
        let sub_data = terminal_only_data();
        let prepared = decoder
            .prepare_rect(&record(&sub_modes, &sub_data), 8, 0, 8, 8, 16, 8)
            .unwrap();
        decoder.commit(prepared);

        assert_eq!(coefficient_seed_snapshot(&decoder, 0), Some(first));
        assert_eq!(coefficient_seed_snapshot(&decoder, 1), Some(second));
    }

    #[test]
    fn slice_b_apple_chroma_ac_table_locks_counts_and_hand_literal_tokens() {
        assert_eq!(APPLE_LUMA_AC_BITS.len(), 16);
        assert_eq!(APPLE_LUMA_AC_VALUES.len(), 162);
        assert_eq!(
            APPLE_LUMA_AC_BITS
                .iter()
                .map(|&value| usize::from(value))
                .sum::<usize>(),
            162
        );
        assert_eq!(
            &APPLE_LUMA_AC_VALUES[..6],
            &[0x01, 0x02, 0x03, 0x00, 0x04, 0x11]
        );
        assert_eq!(APPLE_LUMA_AC_VALUES[31], 0xf0);
        assert_eq!(APPLE_CHROMA_AC_BITS.len(), 16);
        assert_eq!(APPLE_CHROMA_AC_VALUES.len(), 162);
        assert_eq!(
            APPLE_CHROMA_AC_BITS
                .iter()
                .map(|&value| usize::from(value))
                .sum::<usize>(),
            162
        );
        assert_eq!(
            &APPLE_CHROMA_AC_VALUES[..5],
            &[0x00, 0x01, 0x02, 0x03, 0x11]
        );
        assert_eq!(APPLE_CHROMA_AC_VALUES[31], 0xf0);

        let mut writer = TestBitWriter::new();
        // Canonical chroma AC: 1011 = run 1 / size 1, level bit 0 = -1,
        // then 00 = EOB. The value belongs at scan index 2 -> natural 8.
        writer.write_bit_string("1011_0_00");
        let bytes = writer.finish();
        let mut reader = BitReader::new(&bytes);
        let mut coefficients = [0i16; 64];
        decode_partial_ac(
            &mut reader,
            &mut coefficients,
            1,
            3,
            &APPLE_CHROMA_AC_HUFFMAN,
        )
        .unwrap();
        assert_eq!(coefficients[8], -1);
        assert_eq!(coefficients.iter().filter(|&&value| value != 0).count(), 1);
        assert_eq!(reader.bit_position(), 7);

        let mut luma_writer = TestBitWriter::new();
        // Canonical luma AC: 00 = run 0 / size 1, level bit 1 = +1,
        // then 1010 = EOB.
        luma_writer.write_bit_string("00_1_1010");
        let luma_bytes = luma_writer.finish();
        let mut luma_reader = BitReader::new(&luma_bytes);
        let mut luma = [0i16; 64];
        decode_partial_ac(&mut luma_reader, &mut luma, 1, 2, &APPLE_LUMA_AC_HUFFMAN).unwrap();
        assert_eq!(luma[APPLE_NATURAL_ORDER[1]], 1);
        assert_eq!(luma_reader.bit_position(), 7);
    }

    #[test]
    fn partial_ac_nonzero_token_may_cross_extent_within_natural_order_table() {
        static BITS: [u8; 16] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        static VALUES: [u8; 162] = {
            let mut values = [0u8; 162];
            values[0] = 0xa1;
            values[1] = 0x71;
            values
        };
        let table = AppleHuffmanTable {
            bits: &BITS,
            values: &VALUES,
        };
        let mut writer = TestBitWriter::new();
        // 0 = run 10 / size 1, then 1 = run 7 / size 1. Both levels are +1.
        // The second token reproduces cursor 11, extent 14, target 18.
        writer.write_bit_string("0_1_1_1");
        let bytes = writer.finish();
        let mut reader = BitReader::new(&bytes);
        let mut coefficients = [0i16; 64];

        decode_partial_ac(&mut reader, &mut coefficients, 1, 14, &table).unwrap();

        assert_eq!(coefficients[APPLE_NATURAL_ORDER[11]], 1);
        assert_eq!(coefficients[APPLE_NATURAL_ORDER[19]], 1);
        assert_eq!(coefficients.iter().filter(|&&value| value != 0).count(), 2);
        assert_eq!(reader.bit_position(), 4);
    }

    #[test]
    fn partial_ac_zrl_may_cross_extent_without_consuming_another_token() {
        static BITS: [u8; 16] = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        static VALUES: [u8; 162] = {
            let mut values = [0u8; 162];
            values[0] = 0xf0;
            values
        };
        let table = AppleHuffmanTable {
            bits: &BITS,
            values: &VALUES,
        };
        let mut writer = TestBitWriter::new();
        writer.write_bit_string("0_1");
        let bytes = writer.finish();
        let mut reader = BitReader::new(&bytes);
        let mut coefficients = [0i16; 64];

        decode_partial_ac(&mut reader, &mut coefficients, 1, 14, &table).unwrap();

        assert!(coefficients.iter().all(|&value| value == 0));
        assert_eq!(reader.bit_position(), 1);
    }

    #[test]
    fn partial_ac_rejects_nonzero_target_outside_natural_order_table() {
        static BITS: [u8; 16] = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        static VALUES: [u8; 162] = {
            let mut values = [0u8; 162];
            values[0] = 0xf1;
            values
        };
        let table = AppleHuffmanTable {
            bits: &BITS,
            values: &VALUES,
        };
        let mut writer = TestBitWriter::new();
        writer.write_bit_string("0_1");
        let bytes = writer.finish();
        let mut reader = BitReader::new(&bytes);
        let mut coefficients = [0i16; 64];

        let error = decode_partial_ac(&mut reader, &mut coefficients, 49, 16, &table)
            .unwrap_err()
            .to_string();

        assert!(error.contains("scan 索引越界"));
        assert!(coefficients.iter().all(|&value| value == 0));
        assert_eq!(reader.bit_position(), 2);
    }

    #[test]
    fn slice_b_zrl_skips_exactly_sixteen_then_consumes_nonzero_or_eob() {
        let mut nonzero_writer = TestBitWriter::new();
        // Canonical chroma AC: 1111111010 = ZRL, 01/1 = +1, 00 = EOB.
        nonzero_writer.write_bit_string("1111111010_01_1_00");
        let nonzero_bytes = nonzero_writer.finish();
        let mut nonzero_reader = BitReader::new(&nonzero_bytes);
        let mut nonzero = [0i16; 64];
        decode_partial_ac(
            &mut nonzero_reader,
            &mut nonzero,
            1,
            18,
            &APPLE_CHROMA_AC_HUFFMAN,
        )
        .unwrap();
        assert_eq!(nonzero[APPLE_NATURAL_ORDER[17]], 1);
        assert_eq!(nonzero_reader.bit_position(), 15);

        let mut eob_writer = TestBitWriter::new();
        eob_writer.write_bit_string("1111111010_00");
        let eob_bytes = eob_writer.finish();
        let mut eob_reader = BitReader::new(&eob_bytes);
        let mut eob = [0i16; 64];
        decode_partial_ac(&mut eob_reader, &mut eob, 1, 17, &APPLE_CHROMA_AC_HUFFMAN).unwrap();
        assert!(eob.iter().all(|&value| value == 0));
        assert_eq!(eob_reader.bit_position(), 12);
    }

    #[test]
    fn slice_b_opcode_zero_and_exact_bit_cursor_terminals_change_no_state() {
        let mut decoder = decoder_with_tables(1, 1);
        let full = decoder
            .prepare(
                &record(&mode_stream(&[(0, 0)]), &terminal_only_data()),
                8,
                8,
            )
            .unwrap();
        decoder.commit(full);
        let before = cache_index_snapshot(&decoder);
        let partial = partial_payload(0, 0, &["00"]);

        let prepared = decoder.prepare_partial(&partial, 0, 0, 8, 8, 8, 8).unwrap();
        assert_eq!(cache_index_snapshot(&decoder), before);
        decoder.commit_opaque(prepared);
        assert_eq!(cache_index_snapshot(&decoder), before);
        assert_eq!(cache_entry_snapshot(&decoder, 1), None);
    }

    #[test]
    fn type_one_tile_failure_reports_rect_local_global_opcode_and_bit_cursors() {
        let mut decoder = decoder_with_tables(1, 1);
        let full = decoder
            .prepare(
                &record(&mode_stream(&[(0, 11)]), &terminal_only_data()),
                32,
                24,
            )
            .unwrap();
        decoder.commit(full);
        let partial = partial_payload(0, 0, &["00", "01"]);

        let error = decoder
            .prepare_partial(&partial, 8, 8, 16, 8, 32, 24)
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "MVS type-1 tile 处理失败: rect=(8,8,16,8) local_index=1 local_row=0 local_col=1 global_index=6 global_row=1 global_col=2 opcode=1 opcode_start_bit=2 failure_bit=4"
        );
        assert!(format!("{error:#}").contains("MVS type-1 opcode 1 缺少 full-frame 系数 seed"));
    }

    #[test]
    fn type_one_opcode_read_failure_reports_tile_without_inventing_opcode() {
        let mut decoder = decoder_with_tables(1, 1);
        let full = decoder
            .prepare(
                &record(&mode_stream(&[(0, 0)]), &terminal_only_data()),
                8,
                8,
            )
            .unwrap();
        decoder.commit(full);

        let error = decoder
            .prepare_partial(&[1, 0, 0], 0, 0, 8, 8, 8, 8)
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "MVS type-1 opcode 读取失败: rect=(0,0,8,8) local_index=0 local_row=0 local_col=0 global_index=0 global_row=0 global_col=0 opcode_start_bit=0 failure_bit=0"
        );
        let chain = format!("{error:#}");
        assert!(chain.contains("MVS type-1 opcode 不完整"));
        assert!(!error.to_string().contains("opcode="));
    }

    #[test]
    fn type_one_terminal_failure_reports_rect_and_terminal_cursor_without_tile() {
        let mut decoder = decoder_with_tables(1, 1);
        let full = decoder
            .prepare(
                &record(&mode_stream(&[(0, 0)]), &terminal_only_data()),
                8,
                8,
            )
            .unwrap();
        decoder.commit(full);
        let partial = partial_payload_with_terminals(0, 0, &["00"], [0x6d, 0x76, 0x72]);

        let error = decoder
            .prepare_partial(&partial, 0, 0, 8, 8, 8, 8)
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "MVS type-1 terminal 失败: rect=(0,0,8,8) terminal_index=2 terminal_bit=18 failure_bit=26"
        );
        assert!(format!("{error:#}").contains("MVS type-1 位流终止符非法: 0x72"));
        assert!(!error.to_string().contains("tile"));
    }

    #[test]
    fn slice_b_minimal_opcode_one_stages_exact_slot_after_idct_then_commit() {
        let mut decoder = decoder_with_tables(1, 1);
        seed_asymmetric_surface(&mut decoder, 8, 8);
        let partial = partial_payload(1, 1, &["01_000000_0_00_0_00"]);
        clear_partial_order_trace();

        let prepared = decoder.prepare_partial(&partial, 0, 0, 8, 8, 8, 8).unwrap();
        assert_eq!(cache_entry_snapshot(&decoder, 1), None);
        assert_eq!(
            take_partial_order_trace(),
            vec![
                PartialOrderEvent::IdctScratch,
                PartialOrderEvent::CachePopulation
            ]
        );
        decoder.commit_opaque(prepared);

        let mut expected = [0i8; 99];
        expected[0] = 80;
        expected[64] = 80;
        expected[79] = -48;
        assert_eq!(cache_entry_snapshot(&decoder, 1), Some(expected));
        assert_eq!(cache_index_snapshot(&decoder), (0, 1, 1));
    }

    #[test]
    fn slice_b_non_eob_ac_locks_run_sign_natural_order_and_exact_cache_byte() {
        let mut decoder = decoder_with_tables(1, 1);
        seed_asymmetric_surface(&mut decoder, 8, 8);
        // Cb: unchanged DC, run1/size1 -1, EOB. Cr: unchanged DC, EOB.
        let partial = partial_payload(3, 1, &["01_000000_0_1011_0_00_0_00"]);
        let prepared = decoder.prepare_partial(&partial, 0, 0, 8, 8, 8, 8).unwrap();
        decoder.commit_opaque(prepared);

        let entry = cache_entry_snapshot(&decoder, 1).unwrap();
        assert_eq!(entry[64], 80);
        assert_eq!(entry[65], 0);
        assert_eq!(entry[66], -1);
        assert_eq!(entry[79], -48);
    }

    #[test]
    fn slice_b_natural_order_overflow_and_bad_terminal_roll_back_earlier_population() {
        let mut decoder = decoder_with_tables(1, 1);
        seed_asymmetric_surface(&mut decoder, 16, 8);
        let first = "01_000000_0_00_0_00";
        let overflow = concat!(
            "01_000000_0_00_0_",
            "1111111010_1111111010_1111111010_11111111100000_1"
        );
        let bad_natural_order = partial_payload(1, 64, &[first, overflow]);
        assert!(decoder
            .prepare_partial(&bad_natural_order, 0, 0, 16, 8, 16, 8)
            .is_err());
        assert_eq!(cache_index_snapshot(&decoder), (0, 0, 0));
        assert_eq!(cache_entry_snapshot(&decoder, 1), None);

        let bad_terminal =
            partial_payload_with_terminals(1, 1, &[first, first], [0x6d, 0x76, 0x72]);
        assert!(decoder
            .prepare_partial(&bad_terminal, 0, 0, 16, 8, 16, 8)
            .is_err());
        assert_eq!(cache_index_snapshot(&decoder), (0, 0, 0));
        assert_eq!(cache_entry_snapshot(&decoder, 1), None);
    }

    #[test]
    fn slice_b_bad_saved_chroma_count_fails_before_population() {
        let mut decoder = decoder_with_tables(1, 1);
        seed_asymmetric_surface(&mut decoder, 8, 8);
        decoder.surface_state.as_mut().unwrap().tiles[0]
            .coefficients
            .as_mut()
            .unwrap()
            .cb_count = 2;
        let partial = partial_payload(1, 1, &["01_000000_0_00_0_00"]);

        assert!(decoder.prepare_partial(&partial, 0, 0, 8, 8, 8, 8).is_err());
        assert_eq!(cache_index_snapshot(&decoder), (0, 0, 0));
        assert_eq!(cache_entry_snapshot(&decoder, 1), None);
    }

    #[test]
    fn slice_b_opcode_two_validates_reference_without_cache_side_effects() {
        let modes = mode_stream(&[(0, 0), (1, 0)]);
        let data = terminal_only_data();
        let mut decoder = decoder_with_tables(1, 1);
        let full = decoder.prepare(&record(&modes, &data), 16, 8).unwrap();
        decoder.commit(full);
        let partial = partial_payload(0, 0, &["00_10"]);

        let prepared = decoder
            .prepare_partial(&partial, 0, 0, 16, 8, 16, 8)
            .unwrap();
        decoder.commit_opaque(prepared);
        assert_eq!(cache_index_snapshot(&decoder), (0, 0, 0));
        assert_eq!(cache_entry_snapshot(&decoder, 1), None);

        decoder.surface_state.as_mut().unwrap().tiles[0].generation += 1;
        let prepared = decoder
            .prepare_partial(&partial, 0, 0, 16, 8, 16, 8)
            .unwrap();
        decoder.commit_opaque(prepared);
        assert_eq!(cache_index_snapshot(&decoder), (0, 0, 0));
    }

    #[test]
    fn slice_e_opcode_two_missing_reference_and_generation_mismatch_are_opaque_noops() {
        let mut missing = decoder_with_tables(1, 1);
        let full = missing
            .prepare(
                &record(&mode_stream(&[(0, 0)]), &terminal_only_data()),
                8,
                8,
            )
            .unwrap();
        missing.commit(full);
        let missing_reference = partial_payload(0, 0, &["10"]);
        let prepared = missing
            .prepare_partial(&missing_reference, 0, 0, 8, 8, 8, 8)
            .unwrap();
        missing.commit_opaque(prepared);
        assert_eq!(cache_index_snapshot(&missing), (0, 0, 0));

        let mut mismatched = decoder_with_tables(1, 1);
        let full = mismatched
            .prepare(
                &record(&mode_stream(&[(0, 0), (1, 0)]), &terminal_only_data()),
                16,
                8,
            )
            .unwrap();
        mismatched.commit(full);
        mismatched.surface_state.as_mut().unwrap().tiles[0].generation += 1;
        let generation_mismatch = partial_payload(0, 0, &["00_10"]);
        let prepared = mismatched
            .prepare_partial(&generation_mismatch, 0, 0, 16, 8, 16, 8)
            .unwrap();
        mismatched.commit_opaque(prepared);
        assert_eq!(cache_index_snapshot(&mismatched), (0, 0, 0));
    }

    #[test]
    fn slice_b_opcode_three_explicit_big_endian_and_previous_plus_one_are_bounded() {
        let mut decoder = decoder_with_tables(1, 1);
        seed_asymmetric_surface(&mut decoder, 16, 8);
        let populate = partial_payload(1, 1, &["01_000000_0_00_0_00", "01_000000_0_00_0_00"]);
        let prepared = decoder
            .prepare_partial(&populate, 0, 0, 16, 8, 16, 8)
            .unwrap();
        decoder.commit_opaque(prepared);
        assert_eq!(cache_index_snapshot(&decoder), (0, 2, 2));

        let explicit_one = partial_payload(0, 0, &["11_0_00000000_00000001"]);
        let prepared = decoder
            .prepare_partial(&explicit_one, 0, 0, 8, 8, 16, 8)
            .unwrap();
        decoder.commit_opaque(prepared);
        assert_eq!(cache_index_snapshot(&decoder), (1, 2, 2));

        let next = partial_payload(0, 0, &["11_1"]);
        let prepared = decoder.prepare_partial(&next, 0, 0, 8, 8, 16, 8).unwrap();
        decoder.commit_opaque(prepared);
        assert_eq!(cache_index_snapshot(&decoder), (2, 2, 2));

        decoder.cache_state.previous_cache_index = u16::MAX;
        let prepared = decoder.prepare_partial(&next, 0, 0, 8, 8, 16, 8).unwrap();
        decoder.commit_opaque(prepared);
        assert_eq!(decoder.cache_state.previous_cache_index, u16::MAX);

        for invalid in ["11_0_00000000_00000000", "11_0_11111101_11101000"] {
            let payload = partial_payload(0, 0, &[invalid]);
            let prepared = decoder
                .prepare_partial(&payload, 0, 0, 8, 8, 16, 8)
                .unwrap();
            decoder.commit_opaque(prepared);
            assert_eq!(cache_index_snapshot(&decoder), (u16::MAX, 2, 2));
        }
    }

    #[test]
    fn slice_e_opcode_three_lookup_miss_is_fail_soft_and_preserves_prior_population() {
        let mut decoder = decoder_with_tables(1, 1);
        seed_asymmetric_surface(&mut decoder, 16, 8);
        let populate_then_missing_lookup =
            partial_payload(1, 1, &["01_000000_0_00_0_00", "11_0_00000000_00000010"]);

        let prepared = decoder
            .prepare_partial(&populate_then_missing_lookup, 0, 0, 16, 8, 16, 8)
            .unwrap();
        decoder.commit_opaque(prepared);

        assert_eq!(cache_index_snapshot(&decoder), (0, 1, 1));
        assert!(cache_entry_snapshot(&decoder, 1).is_some());
        assert_eq!(cache_entry_snapshot(&decoder, 2), None);

        decoder.cache_state.population_count = 3;
        let uninitialized = partial_payload(0, 0, &["11_0_00000000_00000011"]);
        let prepared = decoder
            .prepare_partial(&uninitialized, 0, 0, 8, 8, 16, 8)
            .unwrap();
        decoder.commit_opaque(prepared);
        assert_eq!(cache_index_snapshot(&decoder), (0, 1, 3));
        assert_eq!(cache_entry_snapshot(&decoder, 3), None);

        for miss in [
            "11_0_00000000_00000000",
            "11_0_11111101_11101000",
            "11_0_11111111_11111111",
        ] {
            let payload = partial_payload(0, 0, &[miss]);
            let prepared = decoder
                .prepare_partial(&payload, 0, 0, 8, 8, 16, 8)
                .unwrap();
            decoder.commit_opaque(prepared);
            assert_eq!(cache_index_snapshot(&decoder), (0, 1, 3));
        }
    }

    #[test]
    fn slice_b_population_advances_slot_two_and_wraps_64999_to_overwrite_slot_one() {
        let mut decoder = decoder_with_tables(1, 1);
        seed_asymmetric_surface(&mut decoder, 8, 8);
        let partial = partial_payload(1, 1, &["01_000000_0_00_0_00"]);
        let first = decoder.prepare_partial(&partial, 0, 0, 8, 8, 8, 8).unwrap();
        decoder.commit_opaque(first);
        let slot_one = cache_entry_snapshot(&decoder, 1).unwrap();
        let second = decoder.prepare_partial(&partial, 0, 0, 8, 8, 8, 8).unwrap();
        decoder.commit_opaque(second);
        let slot_two = cache_entry_snapshot(&decoder, 2).unwrap();
        assert_eq!(cache_index_snapshot(&decoder), (0, 2, 2));

        decoder.cache_state.last_insert_index = 64_999;
        decoder.surface_state.as_mut().unwrap().tiles[0]
            .coefficients
            .as_mut()
            .unwrap()
            .cb_dc = 79;
        let wrapped = decoder.prepare_partial(&partial, 0, 0, 8, 8, 8, 8).unwrap();
        decoder.commit_opaque(wrapped);
        let wrapped_slot_one = cache_entry_snapshot(&decoder, 1).unwrap();
        assert_ne!(wrapped_slot_one, slot_one);
        assert_eq!(wrapped_slot_one[64], 79);
        assert_eq!(cache_entry_snapshot(&decoder, 2), Some(slot_two));
        assert_eq!(decoder.cache_state.last_insert_index, 1);
        assert_eq!(decoder.cache_state.population_count, 3);
    }

    #[test]
    fn slice_b_retained_seed_sequences_feed_opcode_one_and_copy_metadata_feeds_opcode_two() {
        for (mode, data) in [
            (0, terminal_only_data()),
            (3, mode_three_zero_bitmap_data()),
            (4, mode_four_reused_single_color_data()),
        ] {
            let mut seeded = decoder_with_tables(1, 1);
            seed_asymmetric_surface(&mut seeded, 8, 8);
            let modes = mode_stream(&[(mode, 0)]);
            let full = seeded.prepare(&record(&modes, &data), 8, 8).unwrap();
            seeded.commit(full);
            let opcode_one = partial_payload(1, 1, &["01_000000_0_00_0_00"]);
            let opaque = seeded
                .prepare_partial(&opcode_one, 0, 0, 8, 8, 8, 8)
                .unwrap();
            seeded.commit_opaque(opaque);
            assert!(cache_entry_snapshot(&seeded, 1).is_some());

            let mut unseeded = decoder_with_tables(1, 1);
            let full = unseeded.prepare(&record(&modes, &data), 8, 8).unwrap();
            unseeded.commit(full);
            assert!(unseeded
                .prepare_partial(&opcode_one, 0, 0, 8, 8, 8, 8)
                .is_err());
        }

        for (width, height, copy_mode) in [(16, 8, 1), (8, 16, 2)] {
            let mut decoder = decoder_with_tables(1, 1);
            seed_asymmetric_surface(&mut decoder, width, height);
            let modes = mode_stream(&[(0, 0), (copy_mode, 0)]);
            let full = decoder
                .prepare(&record(&modes, &terminal_only_data()), width, height)
                .unwrap();
            decoder.commit(full);
            assert!(reference_snapshot(&decoder, 1).is_some());

            let opcode_one = partial_payload(1, 1, &["00", "01_000000_0_00_0_00"]);
            let opaque = decoder
                .prepare_partial(&opcode_one, 0, 0, width, height, width, height)
                .unwrap();
            decoder.commit_opaque(opaque);
            assert!(cache_entry_snapshot(&decoder, 1).is_some());

            let opcode_two = partial_payload(0, 0, &["00", "10"]);
            let opaque = decoder
                .prepare_partial(&opcode_two, 0, 0, width, height, width, height)
                .unwrap();
            decoder.commit_opaque(opaque);
        }
    }

    #[test]
    fn slice_c_mode_six_uses_big_endian_indices_and_distinct_ac_pixels() {
        let mut decoder = decoder_with_tables(64, 1);
        install_cache_entry(&mut decoder, 0x0102, ac_cache_entry(1), 0x0201);
        install_cache_entry(&mut decoder, 0x0201, ac_cache_entry(-1), 0x0201);
        let modes = mode_stream(&[(6, 0), (6, 0)]);
        let data = cache_mode_data(&[0x0102, 0x0201]);

        let prepared = decoder.prepare(&record(&modes, &data), 16, 8).unwrap();
        assert_eq!(cache_index_snapshot(&decoder).0, 0);
        let decoded = MvsFullDecoder::decoded(&prepared);
        let forward = [150, 147, 141, 132, 124, 115, 109, 106];
        let reverse = [106, 109, 115, 124, 132, 141, 147, 150];
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(pixel(decoded, x, y), [forward[x]; 3]);
                assert_eq!(pixel(decoded, x + 8, y), [reverse[x]; 3]);
            }
        }
        decoder.commit(prepared);
        assert_eq!(decoder.cache_state.previous_cache_index, 0x0201);
    }

    #[test]
    fn slice_c_mode_seven_uses_previous_plus_one_including_initial_slot_one() {
        let mut decoder = decoder_with_tables(64, 1);
        install_cache_entry(&mut decoder, 0x0102, ac_cache_entry(1), 0x0103);
        install_cache_entry(&mut decoder, 0x0103, ac_cache_entry(-1), 0x0103);
        let modes = mode_stream(&[(6, 0), (7, 0)]);
        let data = cache_mode_data(&[0x0102]);
        let prepared = decoder.prepare(&record(&modes, &data), 16, 8).unwrap();
        assert_eq!(pixel(MvsFullDecoder::decoded(&prepared), 0, 0), [150; 3]);
        assert_eq!(pixel(MvsFullDecoder::decoded(&prepared), 8, 0), [106; 3]);
        decoder.commit(prepared);
        assert_eq!(decoder.cache_state.previous_cache_index, 0x0103);

        let mut initial = decoder_with_tables(64, 1);
        install_cache_entry(&mut initial, 1, ac_cache_entry(1), 1);
        let prepared = initial
            .prepare(
                &record(&mode_stream(&[(7, 0)]), &terminal_only_data()),
                8,
                8,
            )
            .unwrap();
        assert_eq!(pixel(MvsFullDecoder::decoded(&prepared), 0, 0), [150; 3]);
        initial.commit(prepared);
        assert_eq!(initial.cache_state.previous_cache_index, 1);

        let absent = decoder_with_tables(64, 1);
        assert!(absent
            .prepare(
                &record(&mode_stream(&[(7, 0)]), &terminal_only_data()),
                8,
                8,
            )
            .is_err());
    }

    #[test]
    fn slice_c_invalid_uninitialized_and_count_mismatch_lookups_are_transactional() {
        for index in [0u16, 65_000u32 as u16] {
            let decoder = decoder_with_tables(64, 1);
            let data = cache_mode_data(&[index]);
            assert!(decoder
                .prepare(&record(&mode_stream(&[(6, 0)]), &data), 8, 8)
                .is_err());
            assert_eq!(decoder.cache_state.previous_cache_index, 0);
            assert!(decoder.surface_state.is_none());
        }

        let mut uninitialized = decoder_with_tables(64, 1);
        uninitialized.cache_state.population_count = 2;
        assert!(uninitialized
            .prepare(
                &record(&mode_stream(&[(6, 0)]), &cache_mode_data(&[2])),
                8,
                8,
            )
            .is_err());
        assert_eq!(uninitialized.cache_state.previous_cache_index, 0);

        let mut mismatch = decoder_with_tables(64, 1);
        install_cache_entry(&mut mismatch, 2, ac_cache_entry(1), 1);
        assert!(mismatch
            .prepare(
                &record(&mode_stream(&[(6, 0)]), &cache_mode_data(&[2])),
                8,
                8,
            )
            .is_err());
        assert_eq!(mismatch.cache_state.previous_cache_index, 0);

        let mut earlier_success = decoder_with_tables(64, 1);
        install_cache_entry(&mut earlier_success, 1, ac_cache_entry(1), 1);
        let modes = mode_stream(&[(6, 0), (6, 0)]);
        let data = cache_mode_data(&[1, 0]);
        assert!(earlier_success
            .prepare(&record(&modes, &data), 16, 8)
            .is_err());
        assert_eq!(earlier_success.cache_state.previous_cache_index, 0);
        assert!(earlier_success.surface_state.is_none());
    }

    #[test]
    fn slice_c_edge_tiles_render_logical_eight_by_eight_but_clip_visible_pixels() {
        let mut decoder = decoder_with_tables(64, 1);
        install_cache_entry(&mut decoder, 1, ac_cache_entry(1), 1);
        let modes = mode_stream(&[(6, 3)]);
        let data = cache_mode_data(&[1, 1, 1, 1]);
        let prepared = decoder.prepare(&record(&modes, &data), 10, 9).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);

        assert_eq!(decoded.rgb.len(), 10 * 9 * RGB_CHANNELS);
        assert_eq!(pixel(decoded, 8, 0), [150; 3]);
        assert_eq!(pixel(decoded, 9, 0), [147; 3]);
        assert_eq!(pixel(decoded, 8, 8), [150; 3]);
        assert_eq!(pixel(decoded, 9, 8), [147; 3]);
    }

    #[test]
    fn slice_c_dropped_preparation_does_not_advance_previous_cache_index() {
        let mut decoder = decoder_with_tables(64, 1);
        install_cache_entry(&mut decoder, 1, ac_cache_entry(1), 1);
        let modes = mode_stream(&[(6, 0)]);
        let data = cache_mode_data(&[1]);
        let record = record(&modes, &data);

        let prepared = decoder.prepare(&record, 8, 8).unwrap();
        assert_eq!(decoder.cache_state.previous_cache_index, 0);
        drop(prepared);
        assert_eq!(decoder.cache_state.previous_cache_index, 0);

        let committed = decoder.prepare(&record, 8, 8).unwrap();
        decoder.commit(committed);
        assert_eq!(decoder.cache_state.previous_cache_index, 1);
    }

    #[test]
    fn mode_zero_emits_exact_white_rgb_tile_without_data_fields() {
        let modes = mode_stream(&[(0, 0)]);
        let data = terminal_only_data();
        let prepared = prepare(&decoder(), &modes, &data, 8, 8).unwrap();
        assert_eq!(
            MvsFullDecoder::decoded(&prepared).rgb,
            vec![0xff; 8 * 8 * 3]
        );
    }

    #[test]
    fn mode_one_copies_immediately_left_tile_and_rejects_first_column() {
        let modes = mode_stream(&[(3, 1), (1, 0)]);
        let data = two_asymmetric_mode_three_tiles_data();
        let prepared = prepare(&decoder(), &modes, &data, 24, 8).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        let distractor = complete_tile(decoded, 0, 0);
        let immediate_left = complete_tile(decoded, 8, 0);
        let copied = complete_tile(decoded, 16, 0);
        assert_ne!(immediate_left, distractor);
        assert_eq!(pixel(decoded, 8, 0), [0, 0, 0]);
        assert_eq!(pixel(decoded, 15, 0), [255, 255, 255]);
        assert_eq!(copied, immediate_left);

        let invalid_modes = mode_stream(&[(1, 0)]);
        assert!(prepare(&decoder(), &invalid_modes, &terminal_only_data(), 8, 8,).is_err());
    }

    #[test]
    fn mode_two_copies_immediately_above_tile_and_rejects_first_row() {
        let modes = mode_stream(&[(3, 1), (2, 0)]);
        let data = two_asymmetric_mode_three_tiles_data();
        let prepared = prepare(&decoder(), &modes, &data, 8, 24).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        let distractor = complete_tile(decoded, 0, 0);
        let immediate_above = complete_tile(decoded, 0, 8);
        let copied = complete_tile(decoded, 0, 16);
        assert_ne!(immediate_above, distractor);
        assert_eq!(pixel(decoded, 0, 8), [0, 0, 0]);
        assert_eq!(pixel(decoded, 7, 8), [255, 255, 255]);
        assert_eq!(copied, immediate_above);

        let invalid_modes = mode_stream(&[(2, 0)]);
        assert!(prepare(&decoder(), &invalid_modes, &terminal_only_data(), 8, 8,).is_err());
    }

    #[test]
    fn mode_three_preserves_msb_pixel_row_order_and_black_white_mapping() {
        let modes = mode_stream(&[(3, 0)]);
        let mut data = TestBitWriter::new();
        data.write_bits(0b0100_0000, 8);
        for bitmap in [0b1000_0001, 0b0100_0000, 0, 0, 0, 0, 0] {
            data.write_bits(bitmap, 8);
        }
        data.write_bits(0x6d, 8);
        let data = data.finish();

        let prepared = prepare(&decoder(), &modes, &data, 8, 8).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        assert_eq!(pixel(decoded, 0, 0), [255, 255, 255]);
        assert_eq!(pixel(decoded, 1, 0), [0, 0, 0]);
        assert_eq!(pixel(decoded, 7, 0), [255, 255, 255]);
        assert_eq!(pixel(decoded, 0, 1), [255, 255, 255]);
        assert_eq!(pixel(decoded, 7, 1), [255, 255, 255]);
        assert_eq!(pixel(decoded, 0, 2), [0, 0, 0]);
        assert_eq!(pixel(decoded, 1, 2), [255, 255, 255]);
        assert_eq!(pixel(decoded, 0, 3), [0, 0, 0]);
    }

    #[test]
    fn mode_four_one_color_uses_y_then_cb_then_cr_fields() {
        let modes = mode_stream(&[(4, 0)]);
        let mut data = TestBitWriter::new();
        data.write_bits(0, 1); // one color
        data.write_bits(0, 1); // new palette
        data.write_bits(100, 8);
        data.write_bits(16, 6); // Cb expands to 64
        data.write_bits(48, 6); // Cr expands to 192
        data.write_bits(0x6d, 8);
        let data = data.finish();

        let prepared = prepare(&decoder(), &modes, &data, 8, 8).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        assert!(decoded.rgb.chunks_exact(3).all(|rgb| rgb == [190, 76, 0]));
    }

    #[test]
    fn mode_four_two_color_preserves_palette_and_msb_bitmap_order() {
        let modes = mode_stream(&[(4, 0)]);
        let mut data = TestBitWriter::new();
        data.write_bits(1, 1); // two colors
        data.write_bits(0, 1); // new palette
        data.write_bits(100, 8);
        data.write_bits(16, 6);
        data.write_bits(48, 6);
        data.write_bits(50, 8);
        data.write_bits(48, 6);
        data.write_bits(16, 6);
        data.write_bits(0b0100_0000, 8);
        for bitmap in [0b1000_0001, 0, 0, 0, 0, 0, 0] {
            data.write_bits(bitmap, 8);
        }
        data.write_bits(0x6d, 8);
        let data = data.finish();

        let prepared = prepare(&decoder(), &modes, &data, 8, 8).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        assert_eq!(pixel(decoded, 0, 0), [190, 76, 0]);
        assert_eq!(pixel(decoded, 1, 0), [0, 74, 163]);
        assert_eq!(pixel(decoded, 7, 0), [190, 76, 0]);
        assert_eq!(pixel(decoded, 0, 1), [190, 76, 0]);
        assert_eq!(pixel(decoded, 7, 1), [190, 76, 0]);
        assert_eq!(pixel(decoded, 0, 2), [0, 74, 163]);
    }

    #[test]
    fn repeat_one_expands_to_exactly_two_tiles() {
        let modes = mode_stream(&[(0, 1)]);
        let data = terminal_only_data();
        let prepared = prepare(&decoder(), &modes, &data, 16, 8).unwrap();
        assert_eq!(
            MvsFullDecoder::decoded(&prepared).rgb,
            vec![0xff; 16 * 8 * 3]
        );
    }

    #[test]
    fn edge_tiles_clip_to_exact_ten_by_nine_rgb_allocation() {
        let modes = mode_stream(&[(0, 3)]);
        let data = terminal_only_data();
        let prepared = prepare(&decoder(), &modes, &data, 10, 9).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        assert_eq!((decoded.width, decoded.height), (10, 9));
        assert_eq!(decoded.rgb.len(), 10 * 9 * 3);
        assert!(decoded.rgb.iter().all(|&channel| channel == 255));
    }

    #[test]
    fn dimensions_are_validated_before_allocation() {
        let modes = mode_stream(&[(0, 0)]);
        let data = terminal_only_data();
        let decoder = decoder();
        assert!(prepare(&decoder, &modes, &data, 0, 8).is_err());
        assert!(prepare(&decoder, &modes, &data, 8, 0).is_err());
        assert!(prepare(&decoder, &modes, &data, usize::MAX, 2).is_err());
        assert!(prepare(&decoder, &modes, &data, MAX_MVS_DECODE_PIXELS + 1, 1,).is_err());
    }

    #[test]
    fn repeat_cannot_exceed_remaining_tile_count() {
        let modes = mode_stream(&[(0, 1)]);
        let data = terminal_only_data();
        assert!(prepare(&decoder(), &modes, &data, 8, 8).is_err());
    }

    #[test]
    fn cache_modes_fail_closed_without_required_fields_or_initialized_slot() {
        let data = terminal_only_data();
        for mode in [6, 7] {
            let modes = mode_stream(&[(mode, 0)]);
            assert!(prepare(&decoder(), &modes, &data, 8, 8).is_err());
        }
    }

    #[test]
    fn both_streams_require_exact_terminal_markers() {
        let valid_modes = mode_stream(&[(0, 0)]);
        let valid_data = terminal_only_data();

        let mut missing_mode = TestBitWriter::new();
        missing_mode.write_bits(0, 1);
        missing_mode.write_bits(0, 3);
        write_repeat(&mut missing_mode, 0);
        assert!(prepare(&decoder(), &missing_mode.finish(), &valid_data, 8, 8,).is_err());

        let mut wrong_mode = TestBitWriter::new();
        wrong_mode.write_bits(0, 1);
        wrong_mode.write_bits(0, 3);
        write_repeat(&mut wrong_mode, 0);
        wrong_mode.write_bits(0x6c, 8);
        assert!(prepare(&decoder(), &wrong_mode.finish(), &valid_data, 8, 8).is_err());
        assert!(prepare(&decoder(), &valid_modes, &[], 8, 8).is_err());
        assert!(prepare(&decoder(), &valid_modes, &[0x6c], 8, 8).is_err());
    }

    #[test]
    fn premature_terminal_bits_cannot_replace_tile_fields() {
        let mut premature_mode = TestBitWriter::new();
        premature_mode.write_bits(0, 1);
        premature_mode.write_bits(0x6d, 8);
        let data = terminal_only_data();
        assert!(prepare(&decoder(), &premature_mode.finish(), &data, 8, 8).is_err());

        let modes = mode_stream(&[(4, 0)]);
        assert!(prepare(&decoder(), &modes, &terminal_only_data(), 8, 8).is_err());
    }

    #[test]
    fn unaligned_terminal_is_exact_and_inserted_padding_is_rejected() {
        let modes = mode_stream(&[(0, 0)]);
        let data = terminal_only_data();
        assert!(prepare(&decoder(), &modes, &data, 8, 8).is_ok());

        let mut padded = TestBitWriter::new();
        padded.write_bits(0, 1);
        padded.write_bits(0, 3);
        write_repeat(&mut padded, 0);
        padded.write_bits(0, 1);
        padded.write_bits(0x6d, 8);
        assert!(prepare(&decoder(), &padded.finish(), &data, 8, 8).is_err());
    }

    #[test]
    fn prepare_failure_and_valid_prepared_drop_leave_decoder_unchanged() {
        let modes = mode_stream(&[(0, 0)]);
        let data = terminal_only_data();
        let decoder = decoder();
        let prepared = prepare(&decoder, &modes, &data, 8, 8).unwrap();
        assert_eq!(decoder.committed_records, 0);
        drop(prepared);
        assert_eq!(decoder.committed_records, 0);

        let malformed_data = [0x6c];
        assert!(prepare(&decoder, &modes, &malformed_data, 8, 8).is_err());
        assert_eq!(decoder.committed_records, 0);

        let after_failure = prepare(&decoder, &modes, &data, 8, 8).unwrap();
        let fresh = prepare(&self::decoder(), &modes, &data, 8, 8).unwrap();
        assert_eq!(
            MvsFullDecoder::decoded(&after_failure),
            MvsFullDecoder::decoded(&fresh)
        );
    }

    #[test]
    fn commit_returns_pixels_and_installs_only_staged_state() {
        let modes = mode_stream(&[(0, 0)]);
        let data = terminal_only_data();
        let mut decoder = decoder();
        let prepared = prepare(&decoder, &modes, &data, 8, 8).unwrap();
        assert_eq!(decoder.committed_records, 0);
        assert_eq!(prepared.next_decoder.committed_records, 1);

        let decoded = decoder.commit(prepared);
        assert_eq!(decoded.rgb, vec![0xff; 8 * 8 * 3]);
        assert_eq!(decoder.committed_records, 1);
    }
}
