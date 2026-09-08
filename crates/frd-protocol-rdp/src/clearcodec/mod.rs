//! ClearCodec 严格状态层；生产接线必须另行提供经过验证的 SIMD 和 NSCodec 后端。
//!
//! 依据 MS-RDPEGFX 2.2.4.1、3.3.8.1。使用 IronRDP 的公开顶层、residual、
//! subcodec PDU parser；bands/RLEX 局部解析避免 0.9.0 的 YOn 位布局、
//! 单色 RLEX 和饱和减法问题。协议解析与像素内核保持独立。

use ironrdp::{
    core::ReadCursor,
    pdu::codecs::clearcodec::{
        decode_residual_layer, decode_subcodec_layer, ClearCodecBitmapStream, SubcodecId,
        FLAG_CACHE_RESET,
    },
};
mod kernels;
mod nscodec;

pub(crate) type NativeDecoder = Decoder<kernels::NativeKernel, nscodec::SimdNsCodec>;

/// 任何必需像素/子码流内核不可用时，整个 ClearCodec 能力保持关闭。
pub(crate) fn native_decoder() -> Option<NativeDecoder> {
    let limits = Limits::default();
    Some(Decoder::new(
        kernels::native_kernel()?,
        nscodec::SimdNsCodec::new(limits.max_pixels).ok()?,
        limits,
    ))
}

use std::{collections::BTreeMap, sync::Arc};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Sequence { expected: u8, actual: u8 },
    MissingCache(&'static str),
    ResourceLimit,
    NsCodecUnavailable,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClearCodec: {self:?}")
    }
}
impl std::error::Error for Error {}

/// 调用方已验证切片长度；实现不得越界访问，或假定输入对齐。
/// 所有输出均为不透明 BGRA。协议层没有默认的生产标量实现。
pub trait PixelKernel {
    fn fill_bgra(&self, dst: &mut [u8], color: [u8; 4]);
    fn expand_bgr24(&self, src: &[u8], dst: &mut [u8]);
    fn copy_bgra(&self, src: &[u8], dst: &mut [u8]);
    /// src 为连续 BGRA 列，dst 从第一目的像素至最后像素末尾，stride 为行字节数。
    fn scatter_column(&self, src: &[u8], dst: &mut [u8], stride: usize);
}

/// 每个 NSCodec 区域独立解码。实现不能将失败区域的状态带入下一调用。
/// 返回严格 width*height*3 字节、从上到下/从左到右的 BGR24。
/// 没有可用实现必须返回错误；不得将空白像素作为成功结果。
pub trait NsCodecProvider {
    fn decode_bgr24(&self, data: &[u8], width: u16, height: u16) -> Result<Vec<u8>>;
}
pub struct UnavailableNsCodec;
impl NsCodecProvider for UnavailableNsCodec {
    fn decode_bgr24(&self, _: &[u8], _: u16, _: u16) -> Result<Vec<u8>> {
        Err(Error::NsCodecUnavailable)
    }
}

#[derive(Clone)]
struct Glyph {
    width: u16,
    height: u16,
    pixels: Arc<[u8]>,
}

/// 限制是本地资源策略而非协议尺寸上限，可由宿主的 framebuffer 预算配置。
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_pixels: usize,
    pub max_wire_bytes: usize,
    pub max_pixel_work: usize,
    pub max_coverage_intervals: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_pixels: 16_777_216,
            max_wire_bytes: 16_777_216,
            max_pixel_work: 134_217_728,
            max_coverage_intervals: 1_048_576,
        }
    }
}

pub struct Decoder<K, N> {
    kernel: K,
    ns: N,
    limits: Limits,
    next_sequence: u8,
    glyphs: Vec<Option<Glyph>>,
    full: Vec<Option<Arc<[u8]>>>,
    short: Vec<Option<Arc<[u8]>>>,
    full_cursor: usize,
    short_cursor: usize,
}
impl<K: PixelKernel, N: NsCodecProvider> Decoder<K, N> {
    pub fn new(kernel: K, ns: N, limits: Limits) -> Self {
        Self {
            kernel,
            ns,
            limits,
            next_sequence: 0,
            glyphs: vec![None; 4000],
            full: vec![None; 32768],
            short: vec![None; 16384],
            full_cursor: 0,
            short_cursor: 0,
        }
    }
    /// 新会话/图形管线重建时调用；CACHE_RESET 仅复位 V-Bar 游标，不清空缓存。
    pub fn reset(&mut self) {
        self.next_sequence = 0;
        self.full_cursor = 0;
        self.short_cursor = 0;
        self.glyphs.fill(None);
        self.full.fill(None);
        self.short.fill(None);
    }
    /// 成功才发布像素、缓存和 sequence；错误后调用方必须终止/重新同步图形流。
    /// 仅 CACHE_RESET 且无位图 payload 的控制消息返回 None。
    pub fn decode(&mut self, data: &[u8], width: u16, height: u16) -> Result<Option<Vec<u8>>> {
        if data.len() > self.limits.max_wire_bytes {
            return Err(Error::ResourceLimit);
        }
        let mut cursor = ReadCursor::new(data);
        let stream = ClearCodecBitmapStream::decode(&mut cursor)
            .map_err(|_| Error::Invalid("bitmap stream"))?;
        if !cursor.is_empty() || stream.flags & !7 != 0 {
            return Err(Error::Invalid("flags/trailing bytes"));
        }
        if stream.seq_number != self.next_sequence {
            return Err(Error::Sequence {
                expected: self.next_sequence,
                actual: stream.seq_number,
            });
        }
        let count = usize::from(width)
            .checked_mul(usize::from(height))
            .ok_or(Error::ResourceLimit)?;
        if count > self.limits.max_pixels || count.checked_mul(4).is_none() {
            return Err(Error::ResourceLimit);
        }
        if stream.is_glyph_hit() && !stream.has_glyph_index() {
            return Err(Error::Invalid("glyph hit without index"));
        }
        if let Some(index) = stream.glyph_index {
            if index >= 4000 || count > 1024 || count == 0 {
                return Err(Error::Invalid("glyph index/area"));
            }
        }
        let reset = stream.flags & FLAG_CACHE_RESET != 0;
        if stream.composite.is_none() && !stream.is_glyph_hit() {
            if stream.flags != FLAG_CACHE_RESET {
                return Err(Error::Invalid("missing composite"));
            }
            self.full_cursor = 0;
            self.short_cursor = 0;
            self.next_sequence = self.next_sequence.wrapping_add(1);
            return Ok(None);
        }
        if width == 0 || height == 0 {
            return Err(Error::Invalid("empty bitmap"));
        }
        if stream.is_glyph_hit() {
            let glyph = self.glyphs[usize::from(stream.glyph_index.unwrap())]
                .as_ref()
                .ok_or(Error::MissingCache("glyph"))?;
            if glyph.width != width || glyph.height != height {
                return Err(Error::Invalid("glyph dimensions"));
            }
            let mut output = vec![0; count * 4];
            self.kernel.copy_bgra(&glyph.pixels, &mut output);
            if reset {
                self.full_cursor = 0;
                self.short_cursor = 0;
            }
            self.next_sequence = self.next_sequence.wrapping_add(1);
            return Ok(Some(output));
        }
        let composite = stream
            .composite
            .ok_or(Error::Invalid("missing composite"))?;
        let mut tx = Transaction {
            full: BTreeMap::new(),
            short: BTreeMap::new(),
            full_cursor: if reset { 0 } else { self.full_cursor },
            short_cursor: if reset { 0 } else { self.short_cursor },
        };
        let mut output = vec![0; count * 4];
        let mut coverage = Coverage::new(width, height, self.limits.max_coverage_intervals);
        let mut budget = self.limits.max_pixel_work;
        if !composite.residual_data.is_empty() {
            validate_residual(composite.residual_data)?;
            let runs = decode_residual_layer(composite.residual_data)
                .map_err(|_| Error::Invalid("residual"))?;
            let mut offset = 0usize;
            for run in runs {
                let n = usize::try_from(run.run_length).map_err(|_| Error::ResourceLimit)?;
                let end = offset
                    .checked_add(n)
                    .filter(|v| *v <= count)
                    .ok_or(Error::Invalid("residual overrun"))?;
                charge(&mut budget, n)?;
                self.kernel.fill_bgra(
                    &mut output[offset * 4..end * 4],
                    [run.blue, run.green, run.red, 255],
                );
                offset = end;
            }
            // MS-RDPEGFX 2.2.4.1.1.1 允许 residual 仅覆盖位图前缀。
            if offset == count {
                coverage.complete = true;
            } else {
                let w = usize::from(width);
                coverage.rectangle(0, 0, w, offset / w)?;
                if offset % w != 0 {
                    coverage.rectangle(0, offset / w, offset % w, 1)?;
                }
            }
        }
        self.bands(
            composite.bands_data,
            width,
            height,
            &mut output,
            &mut tx,
            &mut coverage,
            &mut budget,
        )?;
        let subs = decode_subcodec_layer(composite.subcodec_data)
            .map_err(|_| Error::Invalid("subcodec"))?;
        let mut consumed = 0usize;
        for sub in subs {
            consumed = consumed
                .checked_add(13)
                .and_then(|n| n.checked_add(sub.bitmap_data.len()))
                .ok_or(Error::ResourceLimit)?;
            let sw = usize::from(sub.width);
            let sh = usize::from(sub.height);
            if sw == 0 || sh == 0 {
                return Err(Error::Invalid("empty subcodec"));
            }
            let x = usize::from(sub.x_start);
            let y = usize::from(sub.y_start);
            if x + sw > usize::from(width) || y + sh > usize::from(height) {
                return Err(Error::Invalid("subcodec bounds"));
            }
            let n = sw.checked_mul(sh).ok_or(Error::ResourceLimit)?;
            charge(&mut budget, n)?;
            if sub.bitmap_data.len() > n * 3 {
                return Err(Error::Invalid("subcodec compressed length"));
            }
            let mut pixels = vec![0; n * 4];
            match sub.codec_id {
                SubcodecId::Raw => {
                    if sub.bitmap_data.len() != n * 3 {
                        return Err(Error::Invalid("raw length"));
                    }
                    self.kernel.expand_bgr24(sub.bitmap_data, &mut pixels);
                }
                SubcodecId::NsCodec => {
                    let bgr = self
                        .ns
                        .decode_bgr24(sub.bitmap_data, sub.width, sub.height)?;
                    if bgr.len() != n * 3 {
                        return Err(Error::Invalid("NSCodec output length"));
                    }
                    self.kernel.expand_bgr24(&bgr, &mut pixels);
                }
                SubcodecId::Rlex => self.rlex(sub.bitmap_data, &mut pixels)?,
            }
            for row in 0..sh {
                let dst = ((y + row) * usize::from(width) + x) * 4;
                self.kernel.copy_bgra(
                    &pixels[row * sw * 4..(row + 1) * sw * 4],
                    &mut output[dst..dst + sw * 4],
                );
            }
            coverage.rectangle(x, y, sw, sh)?;
        }
        if consumed != composite.subcodec_data.len() {
            return Err(Error::Invalid("subcodec trailing bytes"));
        }
        if !coverage.is_complete() {
            return Err(Error::Invalid("incomplete bitmap coverage"));
        }
        for (i, p) in tx.full {
            self.full[i] = Some(p);
        }
        for (i, p) in tx.short {
            self.short[i] = Some(p);
        }
        self.full_cursor = tx.full_cursor;
        self.short_cursor = tx.short_cursor;
        if let Some(index) = stream.glyph_index {
            let mut pixels = vec![0; output.len()];
            self.kernel.copy_bgra(&output, &mut pixels);
            self.glyphs[usize::from(index)] = Some(Glyph {
                width,
                height,
                pixels: pixels.into(),
            });
        }
        self.next_sequence = self.next_sequence.wrapping_add(1);
        Ok(Some(output))
    }

    #[allow(clippy::too_many_arguments)]
    fn bands(
        &self,
        data: &[u8],
        width: u16,
        height: u16,
        output: &mut [u8],
        tx: &mut Transaction,
        coverage: &mut Coverage,
        budget: &mut usize,
    ) -> Result<()> {
        let mut src = Bytes(data);
        while !src.0.is_empty() {
            let x = usize::from(src.u16()?);
            let xe = usize::from(src.u16()?);
            let y = usize::from(src.u16()?);
            let ye = usize::from(src.u16()?);
            let bg = src.take(3)?;
            let color = [bg[0], bg[1], bg[2], 255];
            if xe < x
                || ye < y
                || xe >= usize::from(width)
                || ye >= usize::from(height)
                || ye - y >= 52
            {
                return Err(Error::Invalid("band bounds"));
            }
            let h = ye - y + 1;
            charge(budget, (xe - x + 1) * h)?;
            for col in x..=xe {
                let word = src.u16()?;
                let full = if word & 0x8000 != 0 {
                    tx.full
                        .get(&usize::from(word & 0x7fff))
                        .or_else(|| self.full[usize::from(word & 0x7fff)].as_ref())
                        .cloned()
                        .ok_or(Error::MissingCache("full vbar"))?
                } else {
                    let (on, short) = if word & 0x4000 != 0 {
                        let on = usize::from(src.u8()?);
                        let p = tx
                            .short
                            .get(&usize::from(word & 0x3fff))
                            .or_else(|| self.short[usize::from(word & 0x3fff)].as_ref())
                            .cloned()
                            .ok_or(Error::MissingCache("short vbar"))?;
                        (on, p)
                    } else {
                        // MS-RDPEGFX SHORT_VBAR_CACHE_MISS: little-endian YOn(8), YOff(6), x(2).
                        let on = usize::from(word & 255);
                        let off = usize::from((word >> 8) & 63);
                        if off < on || off > h {
                            return Err(Error::Invalid("short vbar bounds"));
                        }
                        let bgr = src.take((off - on) * 3)?;
                        let mut p = vec![0; (off - on) * 4];
                        self.kernel.expand_bgr24(bgr, &mut p);
                        let p: Arc<[u8]> = p.into();
                        tx.short.insert(tx.short_cursor, p.clone());
                        tx.short_cursor = (tx.short_cursor + 1) % 16384;
                        (on, p)
                    };
                    if on + short.len() / 4 > h {
                        return Err(Error::Invalid("cached short vbar bounds"));
                    }
                    let mut p = vec![0; h * 4];
                    self.kernel.fill_bgra(&mut p, color);
                    self.kernel
                        .copy_bgra(&short, &mut p[on * 4..on * 4 + short.len()]);
                    let p: Arc<[u8]> = p.into();
                    tx.full.insert(tx.full_cursor, p.clone());
                    tx.full_cursor = (tx.full_cursor + 1) % 32768;
                    p
                };
                if full.len() != h * 4 {
                    return Err(Error::Invalid("cached full vbar height"));
                }
                let start = (y * usize::from(width) + col) * 4;
                let end = (ye * usize::from(width) + col + 1) * 4;
                self.kernel
                    .scatter_column(&full, &mut output[start..end], usize::from(width) * 4);
            }
            coverage.rectangle(x, y, xe - x + 1, h)?;
        }
        Ok(())
    }
    fn rlex(&self, data: &[u8], output: &mut [u8]) -> Result<()> {
        let mut src = Bytes(data);
        let count = usize::from(src.u8()?);
        if !(1..=127).contains(&count) {
            return Err(Error::Invalid("RLEX palette size"));
        }
        let palette = src.take(count * 3)?;
        // 单色仍有 packed suite 字节。0位 stopIndex，8位 suiteDepth；必须为0。
        let bits = if count == 1 {
            0
        } else {
            usize::BITS - (count - 1).leading_zeros()
        };
        let mask = (1usize << bits) - 1;
        let mut offset = 0usize;
        while !src.0.is_empty() {
            let packed = usize::from(src.u8()?);
            let stop = packed & mask;
            let depth = packed >> bits;
            let start = stop
                .checked_sub(depth)
                .ok_or(Error::Invalid("RLEX suite underflow"))?;
            if stop >= count {
                return Err(Error::Invalid("RLEX palette index"));
            }
            let run = usize::try_from(src.run()?).map_err(|_| Error::ResourceLimit)?;
            let end = offset
                .checked_add(run)
                .and_then(|v| v.checked_add(depth + 1))
                .filter(|v| *v <= output.len() / 4)
                .ok_or(Error::Invalid("RLEX overrun"))?;
            let c = &palette[start * 3..start * 3 + 3];
            self.kernel.fill_bgra(
                &mut output[offset * 4..(offset + run) * 4],
                [c[0], c[1], c[2], 255],
            );
            self.kernel.expand_bgr24(
                &palette[start * 3..(stop + 1) * 3],
                &mut output[(offset + run) * 4..end * 4],
            );
            offset = end;
        }
        if offset != output.len() / 4 {
            return Err(Error::Invalid("RLEX underrun"));
        }
        Ok(())
    }
}
struct Transaction {
    full: BTreeMap<usize, Arc<[u8]>>,
    short: BTreeMap<usize, Arc<[u8]>>,
    full_cursor: usize,
    short_cursor: usize,
}
fn charge(budget: &mut usize, n: usize) -> Result<()> {
    *budget = budget.checked_sub(n).ok_or(Error::ResourceLimit)?;
    Ok(())
}
struct Bytes<'a>(&'a [u8]);
impl<'a> Bytes<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.0.len() {
            return Err(Error::Invalid("truncated layer"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let a = self.take(2)?;
        Ok(u16::from_le_bytes([a[0], a[1]]))
    }
    fn run(&mut self) -> Result<u32> {
        let a = self.u8()?;
        if a < 255 {
            return Ok(u32::from(a));
        }
        let b = self.u16()?;
        if b < 65535 {
            return Ok(u32::from(b));
        }
        let c = self.take(4)?;
        Ok(u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
    }
}
fn validate_residual(data: &[u8]) -> Result<()> {
    let mut src = Bytes(data);
    while !src.0.is_empty() {
        src.take(3)?;
        src.run()?;
    }
    Ok(())
}
/// 覆盖区间是协议元数据；避免用每像素标量循环验证覆盖。
struct Coverage {
    width: usize,
    rows: Vec<Vec<(usize, usize)>>,
    complete: bool,
    remaining_intervals: usize,
}
impl Coverage {
    fn new(w: u16, h: u16, remaining_intervals: usize) -> Self {
        Self {
            width: usize::from(w),
            rows: vec![Vec::new(); usize::from(h)],
            complete: false,
            remaining_intervals,
        }
    }
    fn rectangle(&mut self, x: usize, y: usize, w: usize, h: usize) -> Result<()> {
        if !self.complete {
            charge(&mut self.remaining_intervals, h)?;
            for row in &mut self.rows[y..y + h] {
                row.push((x, x + w));
            }
        }
        Ok(())
    }
    fn is_complete(&mut self) -> bool {
        if self.complete {
            return true;
        }
        self.rows.iter_mut().all(|row| {
            row.sort_unstable();
            let mut end = 0;
            for &(a, b) in row.iter() {
                if a > end {
                    return false;
                }
                end = end.max(b);
            }
            end == self.width
        })
    }
}

#[cfg(test)]
mod tests;
