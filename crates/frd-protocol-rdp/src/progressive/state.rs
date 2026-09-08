use super::{Error, Result};
use ironrdp::pdu::codecs::rfx::progressive::{
    ComponentCodecQuant, ProgressiveBlock, ProgressiveRegion, ProgressiveTile,
};
use std::{collections::BTreeMap, sync::Arc};

pub const TILE_PIXELS: usize = 64 * 64;
pub const TILE_BYTES: usize = TILE_PIXELS * 4;
type TileKey = (u16, u16);
type ContextKey = (u16, u32);

/// 完整 native backend 才能用于生产；测试 oracle 不提供生产构造入口。
/// STATE_BYTES/REFERENCE_BYTES 分别覆盖 DAS/DecDwtQ 的递归堆分配上界。
/// WORKSPACE_BYTES 覆盖一次解码的额外 heap 与 stack 临时峰值。
pub trait TileDecoder {
    type ReferenceState;
    type TileState;
    const REFERENCE_BYTES: usize;
    const STATE_BYTES: usize;
    const WORKSPACE_BYTES: usize = 0;
    fn decode_tile(
        &self,
        reference: Option<&Self::ReferenceState>,
        previous: Option<&Self::TileState>,
        request: TileRequest<'_>,
    ) -> Result<(Self::ReferenceState, Self::TileState, Vec<u8>)>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileParameters {
    pub base: [ComponentCodecQuant; 3],
    pub progressive: [ComponentCodecQuant; 3],
    pub subband_diffing: bool,
    pub reduce_extrapolate: bool,
    pub difference: bool,
}
/// 数据借用 wire parser 输出；original first 时 previous 仍可存在，backend 必须重置 DAS。
pub struct TileRequest<'a> {
    pub tile: &'a ProgressiveTile<'a>,
    pub parameters: &'a TileParameters,
    pub previous_parameters: Option<&'a TileParameters>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}
impl Rect {
    fn right(self) -> u32 {
        u32::from(self.x) + u32::from(self.width)
    }
    fn bottom(self) -> u32 {
        u32::from(self.y) + u32::from(self.height)
    }
}
/// 精确 region∩tile；像素仍是 64×64 BGRA，调用方仅发布 clip。
#[derive(Clone, Debug)]
pub struct TileUpdate {
    pub x: u16,
    pub y: u16,
    pub clip: Rect,
    pub bgra: Arc<[u8]>,
}
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_contexts: usize,
    pub max_tiles: usize,
    pub max_frame_tiles: usize,
    pub max_frame_rects: usize,
    pub max_updates_per_call: usize,
    pub max_decode_tiles_per_call: usize,
    pub max_bytes: usize,
    pub max_surface_pixels: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_contexts: 64,
            max_tiles: 4096,
            max_frame_tiles: 4096,
            max_frame_rects: 4096,
            max_updates_per_call: 8192,
            max_decode_tiles_per_call: 4096,
            max_bytes: 512 * 1024 * 1024,
            max_surface_pixels: 32 * 1024 * 1024,
        }
    }
}
struct CachedTile<S> {
    state: S,
    parameters: TileParameters,
}
struct Context<S> {
    subband_diffing: bool,
    codec_frame: Option<(usize, usize)>,
    tiles: BTreeMap<TileKey, Arc<CachedTile<S>>>,
}
impl<S> Clone for Context<S> {
    fn clone(&self) -> Self {
        Self {
            subband_diffing: self.subband_diffing,
            codec_frame: self.codec_frame,
            tiles: self.tiles.clone(),
        }
    }
}
struct Reference<R> {
    state: R,
    reduce_extrapolate: bool,
}
struct Surface<R> {
    width: u16,
    height: u16,
    references: BTreeMap<TileKey, Arc<Reference<R>>>,
    frame_tiles: BTreeMap<TileKey, Arc<[u8]>>,
    frame_rects: Vec<Rect>,
}
impl<R> Clone for Surface<R> {
    fn clone(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            references: self.references.clone(),
            frame_tiles: self.frame_tiles.clone(),
            frame_rects: self.frame_rects.clone(),
        }
    }
}

pub struct Decoder<K: TileDecoder> {
    kernel: K,
    limits: Limits,
    contexts: BTreeMap<ContextKey, Context<K::TileState>>,
    surfaces: BTreeMap<u16, Surface<K::ReferenceState>>,
    frame: Option<u32>,
}
impl<K: TileDecoder> Decoder<K> {
    pub fn new(kernel: K, limits: Limits) -> Self {
        Self {
            kernel,
            limits,
            contexts: BTreeMap::new(),
            surfaces: BTreeMap::new(),
            frame: None,
        }
    }
    /// 外层 EGFX frame 生命周期；不能用 codec 内层 frame_index 代替。
    pub fn begin_frame(&mut self, frame_id: u32) -> Result<()> {
        if self.frame.is_some() {
            return Err(Error::Invalid("nested EGFX frame"));
        }
        self.clear_frame();
        self.frame = Some(frame_id);
        Ok(())
    }
    pub fn end_frame(&mut self, frame_id: u32) -> Result<()> {
        if self.frame != Some(frame_id) {
            return Err(Error::Invalid("EGFX frame mismatch"));
        }
        if self.contexts.values().any(|c| c.codec_frame.is_some()) {
            return Err(Error::Invalid("unfinished codec frame"));
        }
        self.clear_frame();
        self.frame = None;
        Ok(())
    }
    fn clear_frame(&mut self) {
        for context in self.surfaces.values_mut() {
            context.frame_tiles.clear();
            context.frame_rects.clear();
        }
    }
    pub fn delete_context(&mut self, surface_id: u16, codec_context_id: u32) {
        self.contexts.remove(&(surface_id, codec_context_id));
    }
    pub fn delete_surface(&mut self, surface_id: u16) {
        self.surfaces.remove(&surface_id);
        self.contexts
            .retain(|(surface, _), _| *surface != surface_id);
    }
    pub fn reset(&mut self) {
        self.contexts.clear();
        self.surfaces.clear();
        self.frame = None;
    }
    pub fn context_count(&self) -> usize {
        self.contexts.len()
    }
    pub fn tile_count(&self) -> usize {
        self.surfaces.values().map(|s| s.references.len()).sum()
    }

    /// 整个 bitmap payload 在私有 stage 解码，全部合法才替换 context 并返回可发布更新。
    pub fn decode(
        &mut self,
        surface_id: u16,
        codec_context_id: u32,
        width: u16,
        height: u16,
        blocks: &[ProgressiveBlock<'_>],
    ) -> Result<Vec<TileUpdate>> {
        if self.frame.is_none() {
            return Err(Error::Invalid("missing EGFX frame"));
        }
        let pixels = usize::from(width)
            .checked_mul(usize::from(height))
            .ok_or(Error::ResourceLimit)?;
        if pixels == 0 || pixels > self.limits.max_surface_pixels {
            return Err(Error::ResourceLimit);
        }
        let key = (surface_id, codec_context_id);
        let old = self.contexts.get(&key);
        if old.is_none() && self.contexts.len() >= self.limits.max_contexts {
            return Err(Error::ResourceLimit);
        }
        let mut stage = old.cloned().unwrap_or_else(|| Context {
            subband_diffing: false,
            codec_frame: None,
            tiles: BTreeMap::new(),
        });
        let mut surface = match self.surfaces.get(&surface_id) {
            Some(s) if s.width == width && s.height == height => s.clone(),
            Some(_) => return Err(Error::Invalid("surface dimensions changed without reset")),
            None => {
                if self.surfaces.len() >= self.limits.max_contexts {
                    return Err(Error::ResourceLimit);
                }
                Surface {
                    width,
                    height,
                    references: BTreeMap::new(),
                    frame_tiles: BTreeMap::new(),
                    frame_rects: Vec::new(),
                }
            }
        };
        let resident_tiles = self.tile_count();
        let old_tiles = surface.references.len();
        let mut updates = Vec::new();
        let mut decoded = 0usize;
        self.check_budget(&stage, &surface, 0, 0)?;
        for block in blocks {
            match block {
                ProgressiveBlock::Sync(_) => {} // 可选，wire 已验证 envelope。
                ProgressiveBlock::Context(context) => {
                    if context.tile_size != 64 || context.flags & !1 != 0 {
                        return Err(Error::Invalid("context fields"));
                    }
                    stage.subband_diffing = context.flags & 1 != 0;
                }
                ProgressiveBlock::FrameBegin(begin) => {
                    if stage.codec_frame.is_some() {
                        return Err(Error::Invalid("nested codec frame begin"));
                    }
                    stage.codec_frame = Some((usize::from(begin.region_count), 0));
                }
                ProgressiveBlock::FrameEnd(_) => match stage.codec_frame {
                    Some((expected, seen)) if expected == seen => stage.codec_frame = None,
                    _ => return Err(Error::Invalid("codec frame region count")),
                },
                ProgressiveBlock::Region(region) => {
                    // MS-RDPEGFX 2.2.4.2.1.5 要求忽略 codec frame 外的 region。
                    let Some((expected, seen)) = stage.codec_frame else {
                        continue;
                    };
                    if seen >= expected {
                        return Err(Error::Invalid("too many regions"));
                    }
                    stage.codec_frame = Some((expected, seen + 1));
                    self.region(
                        &mut stage,
                        &mut surface,
                        region,
                        &mut decoded,
                        resident_tiles,
                        old_tiles,
                        &mut updates,
                    )?;
                }
            }
        }
        self.check_budget(&stage, &surface, decoded, updates.len())?;
        self.contexts.insert(key, stage);
        self.surfaces.insert(surface_id, surface);
        Ok(updates)
    }

    fn check_budget(
        &self,
        stage: &Context<K::TileState>,
        surface: &Surface<K::ReferenceState>,
        decoded: usize,
        updates: usize,
    ) -> Result<()> {
        let state_charge = K::STATE_BYTES
            .checked_add(256)
            .ok_or(Error::ResourceLimit)?;
        let ref_charge = K::REFERENCE_BYTES
            .checked_add(128)
            .ok_or(Error::ResourceLimit)?;
        let tile_charge = state_charge
            .checked_add(ref_charge)
            .and_then(|n| n.checked_add(TILE_BYTES))
            .ok_or(Error::ResourceLimit)?;
        let mut bytes = decoded
            .checked_mul(tile_charge)
            .and_then(|n| n.checked_add(K::WORKSPACE_BYTES))
            .ok_or(Error::ResourceLimit)?;
        for context in self.contexts.values() {
            bytes = bytes
                .checked_add(
                    context
                        .tiles
                        .len()
                        .checked_mul(state_charge)
                        .ok_or(Error::ResourceLimit)?,
                )
                .and_then(|n| n.checked_add(256))
                .ok_or(Error::ResourceLimit)?;
        }
        for resident in self.surfaces.values() {
            bytes = bytes
                .checked_add(
                    resident
                        .references
                        .len()
                        .checked_mul(ref_charge)
                        .ok_or(Error::ResourceLimit)?,
                )
                .and_then(|n| {
                    n.checked_add(resident.frame_tiles.len().checked_mul(TILE_BYTES + 64)?)
                })
                .and_then(|n| n.checked_add(resident.frame_rects.len().checked_mul(64)?))
                .and_then(|n| n.checked_add(256))
                .ok_or(Error::ResourceLimit)?;
        }
        bytes = bytes
            .checked_add(
                stage
                    .tiles
                    .len()
                    .checked_mul(64)
                    .ok_or(Error::ResourceLimit)?,
            )
            .and_then(|n| n.checked_add(surface.references.len().checked_mul(64)?))
            .and_then(|n| n.checked_add(surface.frame_tiles.len().checked_mul(64)?))
            .and_then(|n| n.checked_add(surface.frame_rects.len().checked_mul(64)?))
            .and_then(|n| n.checked_add(updates.checked_mul(128)?))
            .and_then(|n| n.checked_add(512))
            .ok_or(Error::ResourceLimit)?;
        if bytes > self.limits.max_bytes {
            return Err(Error::ResourceLimit);
        }
        Ok(())
    }

    fn region(
        &self,
        stage: &mut Context<K::TileState>,
        surface: &mut Surface<K::ReferenceState>,
        region: &ProgressiveRegion<'_>,
        decoded: &mut usize,
        resident_tiles: usize,
        old_tiles: usize,
        updates: &mut Vec<TileUpdate>,
    ) -> Result<()> {
        if region.tile_size != 64
            || region.flags & !1 != 0
            || region.rects.is_empty()
            || region.quant_vals.len() > 7
        {
            return Err(Error::Invalid("region fields"));
        }
        let rect_count = surface
            .frame_rects
            .len()
            .checked_add(region.rects.len())
            .ok_or(Error::ResourceLimit)?;
        if rect_count > self.limits.max_frame_rects {
            return Err(Error::ResourceLimit);
        }
        for tile in &region.tiles {
            *decoded = decoded.checked_add(1).ok_or(Error::ResourceLimit)?;
            if *decoded > self.limits.max_decode_tiles_per_call {
                return Err(Error::ResourceLimit);
            }
            let coord = (tile.x_idx(), tile.y_idx());
            if u32::from(coord.0) * 64 >= u32::from(surface.width)
                || u32::from(coord.1) * 64 >= u32::from(surface.height)
            {
                return Err(Error::Invalid("tile outside surface"));
            }
            let previous = stage.tiles.get(&coord);
            let reference = surface.references.get(&coord);
            let parameters = parameters(
                region,
                tile,
                stage.subband_diffing,
                previous.map(|v| &v.parameters),
                reference.map(|v| v.reduce_extrapolate),
            )?;
            let count = resident_tiles
                .checked_sub(old_tiles)
                .and_then(|n| n.checked_add(surface.references.len()))
                .and_then(|n| n.checked_add(usize::from(reference.is_none())))
                .ok_or(Error::ResourceLimit)?;
            if count > self.limits.max_tiles {
                return Err(Error::ResourceLimit);
            }
            if !surface.frame_tiles.contains_key(&coord)
                && surface.frame_tiles.len() >= self.limits.max_frame_tiles
            {
                return Err(Error::ResourceLimit);
            }
            self.check_budget(stage, surface, *decoded, updates.len())?;
            let (new_reference, state, pixels) = self.kernel.decode_tile(
                reference.map(|v| &v.state),
                previous.map(|v| &v.state),
                TileRequest {
                    tile,
                    parameters: &parameters,
                    previous_parameters: previous.map(|v| &v.parameters),
                },
            )?;
            if pixels.len() != TILE_BYTES {
                return Err(Error::Backend("tile pixel extent"));
            }
            let pixels: Arc<[u8]> = pixels.into();
            surface.frame_tiles.insert(coord, pixels.clone());
            surface.references.insert(
                coord,
                Arc::new(Reference {
                    state: new_reference,
                    reduce_extrapolate: parameters.reduce_extrapolate,
                }),
            );
            stage
                .tiles
                .insert(coord, Arc::new(CachedTile { state, parameters }));
        }
        for rectangle in &region.rects {
            let rect = Rect {
                x: rectangle.x,
                y: rectangle.y,
                width: rectangle.width,
                height: rectangle.height,
            };
            if rect.width == 0
                || rect.height == 0
                || rect.right() > u32::from(surface.width)
                || rect.bottom() > u32::from(surface.height)
            {
                return Err(Error::Invalid("region rectangle outside surface"));
            }
            if surface.frame_rects.contains(&rect) {
                return Err(Error::Invalid("duplicate frame rectangle"));
            }
            for y in u32::from(rect.y) / 64..=(rect.bottom() - 1) / 64 {
                for x in u32::from(rect.x) / 64..=(rect.right() - 1) / 64 {
                    let key = (u16::try_from(x).unwrap(), u16::try_from(y).unwrap());
                    let pixels = surface
                        .frame_tiles
                        .get(&key)
                        .ok_or(Error::Invalid("region lacks current frame tiles"))?;
                    if updates.len() >= self.limits.max_updates_per_call {
                        return Err(Error::ResourceLimit);
                    }
                    let left = (x * 64).max(u32::from(rect.x));
                    let top = (y * 64).max(u32::from(rect.y));
                    let right = ((x + 1) * 64).min(rect.right());
                    let bottom = ((y + 1) * 64).min(rect.bottom());
                    self.check_budget(stage, surface, *decoded, updates.len() + 1)?;
                    updates.push(TileUpdate {
                        x: u16::try_from(x * 64).unwrap(),
                        y: u16::try_from(y * 64).unwrap(),
                        clip: Rect {
                            x: u16::try_from(left).unwrap(),
                            y: u16::try_from(top).unwrap(),
                            width: u16::try_from(right - left).unwrap(),
                            height: u16::try_from(bottom - top).unwrap(),
                        },
                        bgra: pixels.clone(),
                    });
                }
            }
            surface.frame_rects.push(rect);
        }
        Ok(())
    }
}

fn parameters(
    region: &ProgressiveRegion<'_>,
    tile: &ProgressiveTile<'_>,
    subband_diffing: bool,
    previous: Option<&TileParameters>,
    reference_layout: Option<bool>,
) -> Result<TileParameters> {
    let (indices, quality, flags, upgrade) = match tile {
        ProgressiveTile::Simple(t) => (
            [t.quant_idx_y, t.quant_idx_cb, t.quant_idx_cr],
            255,
            t.flags,
            false,
        ),
        ProgressiveTile::First(t) => (
            [t.quant_idx_y, t.quant_idx_cb, t.quant_idx_cr],
            t.quality,
            t.flags,
            false,
        ),
        ProgressiveTile::Upgrade(t) => (
            [t.quant_idx_y, t.quant_idx_cb, t.quant_idx_cr],
            t.quality,
            0,
            true,
        ),
    };
    // MS-RDPEGFX v20260511 §2.2.4.2.1.5.4（p63）：FIRST高7位必须忽略。
    // SIMPLE §2.2.4.2.1.5.3 没有这项规则，仍只接受已定义的difference位。
    if matches!(tile, ProgressiveTile::Simple(_)) && flags & !1 != 0 {
        return Err(Error::Invalid("tile flags"));
    }
    let difference = if upgrade {
        previous.ok_or(Error::MissingTile)?.difference
    } else {
        flags & 1 != 0
    };
    if difference && (!subband_diffing || reference_layout.is_none()) {
        return Err(Error::MissingTile);
    }
    let mut base = [ComponentCodecQuant::LOSSLESS; 3];
    for i in 0..3 {
        base[i] = *region
            .quant_vals
            .get(usize::from(indices[i]))
            .ok_or(Error::Invalid("quant index"))?;
    }
    let progressive = if quality == 255 {
        [ComponentCodecQuant::LOSSLESS; 3]
    } else {
        let q = region
            .quant_prog_vals
            .get(usize::from(quality))
            .ok_or(Error::Invalid("progressive quant index"))?;
        [q.y_quant, q.cb_quant, q.cr_quant]
    };
    for component in 0..3 {
        for band in 0..10 {
            if base[component].for_band(band) > 15 || progressive[component].for_band(band) > 8 {
                return Err(Error::Invalid("quant value"));
            }
        }
    }
    let parameters = TileParameters {
        base,
        progressive,
        subband_diffing,
        reduce_extrapolate: region.flags & 1 != 0,
        difference,
    };
    if (difference || upgrade)
        && reference_layout.is_some_and(|layout| layout != parameters.reduce_extrapolate)
    {
        return Err(Error::Invalid("difference tile layout changed"));
    }
    if upgrade {
        let previous = previous.ok_or(Error::MissingTile)?;
        if previous.reduce_extrapolate != parameters.reduce_extrapolate
            || previous.subband_diffing != subband_diffing
        {
            return Err(Error::Invalid("upgrade layout or context changed"));
        }
        for c in 0..3 {
            for b in 0..10 {
                let prior = u16::from(previous.base[c].for_band(b))
                    + u16::from(previous.progressive[c].for_band(b));
                let current =
                    u16::from(base[c].for_band(b)) + u16::from(progressive[c].for_band(b));
                if current > prior {
                    return Err(Error::Invalid("upgrade quantization regressed"));
                }
            }
        }
    }
    Ok(parameters)
}
