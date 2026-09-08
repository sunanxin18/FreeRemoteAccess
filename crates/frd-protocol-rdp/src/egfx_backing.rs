//! EGFX 离屏像素存储。所有表面与缓存共享字节预算，像素读写复用已验证 SIMD。
use super::{EgfxCoverage, ExclusiveRectangle};
use crate::clearcodec::{native_kernel, NativeKernel, PixelKernel};
use frd_core::{PixelRect, PixelSize};
use std::collections::BTreeMap;

pub(super) type Result<T> = std::result::Result<T, ()>;
pub(super) const DEFAULT_BUDGET: usize = 256 * 1024 * 1024;
const MAX_SCRATCH_BYTES: usize = 256 * 1024 * 1024;
const MAX_COPY_PIXELS: usize = 64 * 1024 * 1024;

#[derive(Debug)]
struct Image {
    size: PixelSize,
    pixels: Vec<u8>,
    valid: EgfxCoverage,
}
impl Image {
    fn byte_cost(size: PixelSize) -> Option<usize> {
        let n = usize::try_from(u64::from(size.width) * u64::from(size.height)).ok()?;
        n.checked_mul(4)?.checked_add(n.checked_add(63)? / 64 * 8)
    }
    fn new(size: PixelSize) -> Result<Self> {
        let n =
            usize::try_from(u64::from(size.width) * u64::from(size.height) * 4).map_err(|_| ())?;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(n).map_err(|_| ())?;
        pixels.resize(n, 0);
        Ok(Self {
            size,
            pixels,
            valid: EgfxCoverage::try_new(size).ok_or(())?,
        })
    }
    fn check(&self, r: PixelRect) -> Result<()> {
        let (_, end) = r.checked_bounds().ok_or(())?;
        if r.width == 0 || r.height == 0 || end.x > self.size.width || end.y > self.size.height {
            Err(())
        } else {
            Ok(())
        }
    }
    fn known(&self, r: PixelRect) -> Result<()> {
        self.check(r)?;
        for y in r.y..r.y + r.height {
            let mut p = u64::from(y) * u64::from(self.size.width) + u64::from(r.x);
            let end = p + u64::from(r.width);
            while p < end {
                let bit = (p % 64) as u32;
                let n = (end - p).min(u64::from(64 - bit));
                let mask = if n == 64 {
                    u64::MAX
                } else {
                    ((1u64 << n) - 1) << bit
                };
                if self.valid.words[(p / 64) as usize] & mask != mask {
                    return Err(());
                }
                p += n;
            }
        }
        Ok(())
    }
    fn read(&self, r: PixelRect, k: &NativeKernel, scratch_budget: usize) -> Result<Vec<u8>> {
        self.known(r)?;
        let row = r.width as usize * 4;
        let len = row
            .checked_mul(r.height as usize)
            .filter(|n| *n <= scratch_budget)
            .ok_or(())?;
        let mut out = Vec::new();
        out.try_reserve_exact(len).map_err(|_| ())?;
        out.resize(len, 0);
        for y in 0..r.height as usize {
            let at = ((r.y as usize + y) * self.size.width as usize + r.x as usize) * 4;
            k.copy_bgra(&self.pixels[at..at + row], &mut out[y * row..(y + 1) * row]);
        }
        Ok(out)
    }
    fn write(&mut self, r: PixelRect, p: &[u8], k: &NativeKernel) -> Result<()> {
        self.check(r)?;
        let row = r.width as usize * 4;
        if p.len() != row * r.height as usize {
            return Err(());
        }
        for y in 0..r.height as usize {
            let at = ((r.y as usize + y) * self.size.width as usize + r.x as usize) * 4;
            k.copy_bgra(&p[y * row..(y + 1) * row], &mut self.pixels[at..at + row]);
        }
        self.valid.record(r);
        Ok(())
    }
    fn rectangles(&self, limit: usize) -> Result<Vec<PixelRect>> {
        if self.valid.covered_pixels == u64::from(self.size.width) * u64::from(self.size.height) {
            return Ok(vec![PixelRect {
                x: 0,
                y: 0,
                width: self.size.width,
                height: self.size.height,
            }]);
        }
        let mut out: Vec<PixelRect> = Vec::new();
        let mut previous = BTreeMap::new();
        for y in 0..self.size.height {
            let mut current = BTreeMap::new();
            let base = u64::from(y) * u64::from(self.size.width);
            let end = base + u64::from(self.size.width);
            let mut p = base;
            let mut run = None;
            while p < end {
                let bits = self.valid.words[(p / 64) as usize] >> (p % 64);
                let n = (end - p).min(64 - p % 64);
                let len = if bits & 1 == 0 {
                    u64::from(bits.trailing_zeros()).min(n)
                } else {
                    u64::from(bits.trailing_ones()).min(n)
                };
                if bits & 1 != 0 {
                    run.get_or_insert((p - base) as u32);
                } else if let Some(x) = run.take() {
                    Self::append_rect(
                        &mut out,
                        &previous,
                        &mut current,
                        x,
                        y,
                        (p - base) as u32 - x,
                        limit,
                    )?;
                }
                p += len;
            }
            if let Some(x) = run {
                Self::append_rect(
                    &mut out,
                    &previous,
                    &mut current,
                    x,
                    y,
                    self.size.width - x,
                    limit,
                )?;
            }
            previous = current;
        }
        Ok(out)
    }
    fn append_rect(
        out: &mut Vec<PixelRect>,
        previous: &BTreeMap<(u32, u32), usize>,
        current: &mut BTreeMap<(u32, u32), usize>,
        x: u32,
        y: u32,
        w: u32,
        limit: usize,
    ) -> Result<()> {
        let key = (x, w);
        let index = if let Some(&i) = previous.get(&key) {
            out[i].height += 1;
            i
        } else {
            if out.len() >= limit {
                return Err(());
            }
            out.push(PixelRect {
                x,
                y,
                width: w,
                height: 1,
            });
            out.len() - 1
        };
        current.insert(key, index);
        Ok(())
    }
}
pub(super) struct Backings {
    surfaces: BTreeMap<u16, Image>,
    cache: BTreeMap<u16, Image>,
    used: usize,
    budget: usize,
    scratch_budget: usize,
    kernel: NativeKernel,
}
impl std::fmt::Debug for Backings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backings")
            .field("surfaces", &self.surfaces.len())
            .field("cache", &self.cache.len())
            .field("used", &self.used)
            .field("budget", &self.budget)
            .finish()
    }
}
impl Backings {
    pub fn new(budget: usize) -> Option<Self> {
        Some(Self {
            surfaces: BTreeMap::new(),
            cache: BTreeMap::new(),
            used: 0,
            budget,
            scratch_budget: MAX_SCRATCH_BYTES,
            kernel: native_kernel()?,
        })
    }
    pub fn clear(&mut self) {
        self.surfaces.clear();
        self.cache.clear();
        self.used = 0;
    }
    // MS-RDPEGFX 3.3.5.14 仅重置输出；Bitmap Cache 由独立 Evict 生命周期管理。
    pub fn clear_surfaces(&mut self) {
        self.surfaces.clear();
        // 已计费的 cache 是 used 的子集，因此重新求和不会溢出或重复扣费。
        self.used = self
            .cache
            .values()
            .map(|image| Image::byte_cost(image.size).unwrap())
            .sum();
    }
    pub fn contains_cache(&self, slot: u16) -> bool {
        self.cache.contains_key(&slot)
    }
    pub fn create(&mut self, id: u16, size: PixelSize) -> Result<()> {
        if self.surfaces.contains_key(&id) {
            return Err(());
        }
        let bytes = Image::byte_cost(size).ok_or(())?;
        let used = self
            .used
            .checked_add(bytes)
            .filter(|n| *n <= self.budget)
            .ok_or(())?;
        let image = Image::new(size)?;
        self.surfaces.insert(id, image);
        self.used = used;
        Ok(())
    }
    pub fn delete(&mut self, id: u16) {
        if let Some(image) = self.surfaces.remove(&id) {
            self.used -= Image::byte_cost(image.size).unwrap();
        }
    }
    pub fn contains(&self, id: u16) -> bool {
        self.surfaces.contains_key(&id)
    }
    pub fn check(&self, id: u16, r: PixelRect) -> Result<()> {
        self.surfaces.get(&id).ok_or(())?.check(r)
    }
    pub fn write(&mut self, id: u16, r: PixelRect, p: &[u8]) -> Result<()> {
        self.surfaces
            .get_mut(&id)
            .ok_or(())?
            .write(r, p, &self.kernel)
    }
    pub fn read(&self, id: u16, r: PixelRect) -> Result<Vec<u8>> {
        self.surfaces
            .get(&id)
            .ok_or(())?
            .read(r, &self.kernel, self.scratch_budget)
    }
    pub fn valid_rectangles(&self, id: u16, limit: usize) -> Result<Vec<PixelRect>> {
        self.surfaces.get(&id).ok_or(())?.rectangles(limit)
    }
    pub fn fill(&mut self, id: u16, rs: &[PixelRect], c: [u8; 4]) -> Result<()> {
        let image = self.surfaces.get_mut(&id).ok_or(())?;
        let mut total = 0usize;
        for &r in rs {
            image.check(r)?;
            total = total
                .checked_add(r.width as usize * r.height as usize)
                .filter(|n| *n <= MAX_COPY_PIXELS)
                .ok_or(())?;
        }
        for &r in rs {
            for y in r.y..r.y + r.height {
                let at = (y as usize * image.size.width as usize + r.x as usize) * 4;
                self.kernel
                    .fill_bgra(&mut image.pixels[at..at + r.width as usize * 4], c);
            }
            image.valid.record(r);
        }
        Ok(())
    }
    pub fn copy_surface(
        &mut self,
        source: u16,
        r: PixelRect,
        dest: u16,
        points: &[(u16, u16)],
    ) -> Result<Vec<PixelRect>> {
        let output = self.read(source, r)?;
        self.copy_pixels(&output, r.width, r.height, dest, points)
    }
    fn copy_pixels(
        &mut self,
        pixels: &[u8],
        width: u32,
        height: u32,
        dest: u16,
        points: &[(u16, u16)],
    ) -> Result<Vec<PixelRect>> {
        let image = self.surfaces.get_mut(&dest).ok_or(())?;
        let work = (width as usize)
            .checked_mul(height as usize)
            .and_then(|v| v.checked_mul(points.len()))
            .filter(|v| *v <= MAX_COPY_PIXELS)
            .ok_or(())?;
        let _ = work;
        let rs: Vec<_> = points
            .iter()
            .map(|&(x, y)| PixelRect {
                x: u32::from(x),
                y: u32::from(y),
                width,
                height,
            })
            .collect();
        for &r in &rs {
            image.check(r)?;
        }
        // 源快照在写入前取得，同表面重叠移动也保持 memmove 语义。
        for &r in &rs {
            image.write(r, pixels, &self.kernel)?;
        }
        Ok(rs)
    }
    pub fn cache_surface(&mut self, id: u16, r: PixelRect, slot: u16) -> Result<()> {
        if slot >= 4096 {
            return Err(());
        }
        let size = PixelSize::new(r.width, r.height).ok_or(())?;
        let output_bytes = (r.width as usize)
            .checked_mul(r.height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(())?;
        output_bytes
            .checked_add(Image::byte_cost(size).ok_or(())?)
            .filter(|n| *n <= self.scratch_budget)
            .ok_or(())?;
        let old = self
            .cache
            .get(&slot)
            .and_then(|p| Image::byte_cost(p.size))
            .unwrap_or(0);
        let used = self
            .used
            .checked_sub(old)
            .and_then(|n| n.checked_add(Image::byte_cost(size)?))
            .filter(|n| *n <= self.budget)
            .ok_or(())?;
        let pixels = self.read(id, r)?;
        let mut image = Image::new(size)?;
        image.write(
            PixelRect {
                x: 0,
                y: 0,
                width: r.width,
                height: r.height,
            },
            &pixels,
            &self.kernel,
        )?;
        self.cache.insert(slot, image);
        self.used = used;
        Ok(())
    }
    pub fn copy_cache(
        &mut self,
        slot: u16,
        dest: u16,
        points: &[(u16, u16)],
    ) -> Result<Vec<PixelRect>> {
        let image = self.cache.get(&slot).ok_or(())?;
        let size = image.size;
        let pixels = image.read(
            PixelRect {
                x: 0,
                y: 0,
                width: size.width,
                height: size.height,
            },
            &self.kernel,
            self.scratch_budget,
        )?;
        self.copy_pixels(&pixels, size.width, size.height, dest, points)
    }
    pub fn evict(&mut self, slot: u16) -> Result<()> {
        let image = self.cache.remove(&slot).ok_or(())?;
        self.used -= Image::byte_cost(image.size).unwrap();
        Ok(())
    }
}
pub(super) fn rect(r: &ExclusiveRectangle) -> Result<PixelRect> {
    Ok(PixelRect {
        x: u32::from(r.left),
        y: u32::from(r.top),
        width: u32::from(r.right.checked_sub(r.left).filter(|v| *v > 0).ok_or(())?),
        height: u32::from(r.bottom.checked_sub(r.top).filter(|v| *v > 0).ok_or(())?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn r(x: u32, y: u32, w: u32, h: u32) -> PixelRect {
        PixelRect {
            x,
            y,
            width: w,
            height: h,
        }
    }
    #[test]
    fn budget_is_shared_and_delete_reset_evict_return_bytes() {
        let mut b = Backings::new(40).unwrap();
        b.create(1, PixelSize::new(2, 2).unwrap()).unwrap();
        assert!(b.create(2, PixelSize::new(2, 2).unwrap()).is_err());
        b.fill(1, &[r(0, 0, 2, 2)], [1, 2, 3, 255]).unwrap();
        assert!(b.cache_surface(1, r(0, 0, 2, 2), 0).is_err());
        assert_eq!(b.used, 24);
        b.cache_surface(1, r(0, 0, 1, 1), 0).unwrap();
        assert_eq!(b.used, 36);
        b.evict(0).unwrap();
        assert_eq!(b.used, 24);
        b.delete(1);
        assert_eq!(b.used, 0);
        b.create(2, PixelSize::new(2, 2).unwrap()).unwrap();
        b.clear();
        assert_eq!(b.used, 0);
        assert!(!b.contains(2));
    }
    #[test]
    fn repeated_surface_reset_preserves_cache_charge_and_eviction_releases_it() {
        let mut b = Backings::new(48).unwrap();
        let size = PixelSize::new(2, 2).unwrap();
        b.create(1, size).unwrap();
        b.fill(1, &[r(0, 0, 2, 2)], [1, 2, 3, 255]).unwrap();
        b.cache_surface(1, r(0, 0, 2, 2), 3).unwrap();
        assert_eq!(b.used, 48);
        for _ in 0..3 {
            b.clear_surfaces();
            assert_eq!(b.used, 24);
            assert!(b.contains_cache(3));
            b.create(2, size).unwrap();
            assert_eq!(b.used, 48);
            assert!(b.create(4, PixelSize::new(1, 1).unwrap()).is_err());
            b.copy_cache(3, 2, &[(0, 0)]).unwrap();
            assert_eq!(b.read(2, r(0, 0, 2, 2)).unwrap(), [1, 2, 3, 255].repeat(4));
        }
        b.clear_surfaces();
        b.clear_surfaces();
        assert_eq!(b.used, 24);
        b.evict(3).unwrap();
        assert_eq!(b.used, 0);
        assert!(b.copy_cache(3, 2, &[(0, 0)]).is_err());
        b.create(1, size).unwrap();
        b.fill(1, &[r(0, 0, 2, 2)], [1, 2, 3, 255]).unwrap();
        b.cache_surface(1, r(0, 0, 2, 2), 3).unwrap();
        b.clear();
        assert_eq!(b.used, 0);
        assert!(!b.contains_cache(3));
    }
    #[test]
    fn overlapping_copy_uses_snapshot_and_bad_second_destination_is_transactional() {
        let mut b = Backings::new(1024).unwrap();
        b.create(1, PixelSize::new(4, 1).unwrap()).unwrap();
        let pixels = [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
        b.write(1, r(0, 0, 4, 1), &pixels).unwrap();
        assert!(b
            .copy_surface(1, r(0, 0, 3, 1), 1, &[(1, 0), (2, 0)])
            .is_err());
        assert_eq!(b.read(1, r(0, 0, 4, 1)).unwrap(), pixels);
        b.copy_surface(1, r(0, 0, 3, 1), 1, &[(1, 0)]).unwrap();
        assert_eq!(
            b.read(1, r(0, 0, 4, 1)).unwrap(),
            [1, 2, 3, 255, 1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255]
        );
    }
    #[test]
    fn replacement_scratch_budget_rejects_before_replacing_old_cache() {
        let mut b = Backings::new(1024).unwrap();
        b.create(1, PixelSize::new(3, 2).unwrap()).unwrap();
        b.fill(1, &[r(0, 0, 3, 2)], [1, 2, 3, 255]).unwrap();
        b.cache_surface(1, r(0, 0, 2, 2), 0).unwrap();
        let old_used = b.used;
        b.scratch_budget = 40;
        assert!(b.cache_surface(1, r(0, 0, 3, 2), 0).is_err());
        assert_eq!(b.used, old_used);
        assert_eq!(b.cache[&0].size, PixelSize::new(2, 2).unwrap());
        assert_eq!(b.cache[&0].pixels, [1, 2, 3, 255].repeat(4));
    }
    #[test]
    fn partial_validity_and_cache_copy_are_exact() {
        let mut b = Backings::new(1024).unwrap();
        b.create(1, PixelSize::new(3, 2).unwrap()).unwrap();
        b.fill(1, &[r(1, 0, 1, 2)], [2, 4, 6, 255]).unwrap();
        assert_eq!(b.valid_rectangles(1, 4).unwrap(), vec![r(1, 0, 1, 2)]);
        assert!(b.read(1, r(0, 0, 3, 2)).is_err());
        b.cache_surface(1, r(1, 0, 1, 2), 3).unwrap();
        b.create(2, PixelSize::new(1, 2).unwrap()).unwrap();
        b.copy_cache(3, 2, &[(0, 0)]).unwrap();
        assert_eq!(b.read(2, r(0, 0, 1, 2)).unwrap(), [2, 4, 6, 255].repeat(2));
        b.evict(3).unwrap();
        assert!(b.copy_cache(3, 2, &[(0, 0)]).is_err());
    }
}
