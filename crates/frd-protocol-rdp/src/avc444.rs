//! Reference AVC444 YUV420/Chroma420 reconstruction.
//!
//! The pinned IronRDP EGFX client forwards AVC444 and AVC444v2 payloads to the
//! application because it only exposes an RGBA H.264 decoder contract.  This
//! module keeps the wire-specific reconstruction rules separate from that
//! contract.  It is deliberately a bounded reference implementation: it is
//! not registered in the production graphics hot path until an exact planar
//! decoder adapter, target SIMD kernels, and live recovery evidence exist.

use ironrdp::pdu::geometry::InclusiveRectangle;

const MAX_YUV444_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Avc444ChromaLayout {
    V1,
    V2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Avc444ReconstructionMode {
    Luma,
    LumaAndChroma(Avc444ChromaLayout),
    Chroma(Avc444ChromaLayout),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Avc444ReconstructionError {
    InvalidDimensions,
    AllocationOverBudget,
    InvalidPlane,
    RegionOutOfBounds,
    MissingLumaReference,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Yuv420Plane<'a> {
    bytes: &'a [u8],
    stride: usize,
    width: usize,
    height: usize,
}

impl<'a> Yuv420Plane<'a> {
    pub(crate) fn new(
        bytes: &'a [u8],
        stride: usize,
        width: usize,
        height: usize,
    ) -> Result<Self, Avc444ReconstructionError> {
        if width == 0 || height == 0 || stride < width {
            return Err(Avc444ReconstructionError::InvalidPlane);
        }
        let required = stride
            .checked_mul(height)
            .ok_or(Avc444ReconstructionError::InvalidPlane)?;
        if bytes.len() < required {
            return Err(Avc444ReconstructionError::InvalidPlane);
        }
        Ok(Self {
            bytes,
            stride,
            width,
            height,
        })
    }

    fn sample(self, x: usize, y: usize) -> Result<u8, Avc444ReconstructionError> {
        if x >= self.width || y >= self.height {
            return Err(Avc444ReconstructionError::RegionOutOfBounds);
        }
        let offset = y
            .checked_mul(self.stride)
            .and_then(|row| row.checked_add(x))
            .ok_or(Avc444ReconstructionError::RegionOutOfBounds)?;
        self.bytes
            .get(offset)
            .copied()
            .ok_or(Avc444ReconstructionError::RegionOutOfBounds)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Yuv420Frame<'a> {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) y: Yuv420Plane<'a>,
    pub(crate) u: Yuv420Plane<'a>,
    pub(crate) v: Yuv420Plane<'a>,
}

impl<'a> Yuv420Frame<'a> {
    pub(crate) fn try_new(
        width: usize,
        height: usize,
        y: Yuv420Plane<'a>,
        u: Yuv420Plane<'a>,
        v: Yuv420Plane<'a>,
    ) -> Result<Self, Avc444ReconstructionError> {
        if width == 0 || height == 0 {
            return Err(Avc444ReconstructionError::InvalidDimensions);
        }
        let chroma_width = width.div_ceil(2);
        let chroma_height = height.div_ceil(2);
        if y.width < width
            || y.height < height
            || u.width < chroma_width
            || u.height < chroma_height
            || v.width < chroma_width
            || v.height < chroma_height
        {
            return Err(Avc444ReconstructionError::InvalidPlane);
        }
        Ok(Self {
            width,
            height,
            y,
            u,
            v,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Yuv444Frame {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Yuv444Frame {
    pub(crate) fn try_new(width: usize, height: usize) -> Result<Self, Avc444ReconstructionError> {
        if width == 0 || height == 0 {
            return Err(Avc444ReconstructionError::InvalidDimensions);
        }
        let plane_bytes = width
            .checked_mul(height)
            .ok_or(Avc444ReconstructionError::AllocationOverBudget)?;
        let total_bytes = plane_bytes
            .checked_mul(3)
            .ok_or(Avc444ReconstructionError::AllocationOverBudget)?;
        if total_bytes > MAX_YUV444_BYTES {
            return Err(Avc444ReconstructionError::AllocationOverBudget);
        }
        Ok(Self {
            width,
            height,
            y: vec![0; plane_bytes],
            u: vec![0; plane_bytes],
            v: vec![0; plane_bytes],
        })
    }

    pub(crate) fn width(&self) -> usize {
        self.width
    }

    pub(crate) fn height(&self) -> usize {
        self.height
    }

    pub(crate) fn planes(&self) -> [&[u8]; 3] {
        [&self.y, &self.u, &self.v]
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Yuv444Reconstructor {
    frame: Yuv444Frame,
    has_luma_reference: bool,
}

impl Yuv444Reconstructor {
    pub(crate) fn try_new(width: usize, height: usize) -> Result<Self, Avc444ReconstructionError> {
        Ok(Self {
            frame: Yuv444Frame::try_new(width, height)?,
            has_luma_reference: false,
        })
    }

    pub(crate) fn frame(&self) -> &Yuv444Frame {
        &self.frame
    }

    pub(crate) fn apply(
        &mut self,
        mode: Avc444ReconstructionMode,
        luma: &Yuv420Frame<'_>,
        luma_regions: &[InclusiveRectangle],
        chroma: Option<&Yuv420Frame<'_>>,
        chroma_regions: &[InclusiveRectangle],
    ) -> Result<(), Avc444ReconstructionError> {
        if luma.width != self.frame.width || luma.height != self.frame.height {
            return Err(Avc444ReconstructionError::InvalidDimensions);
        }
        let mut candidate = self.frame.clone();
        let mut candidate_has_luma = self.has_luma_reference;
        match mode {
            Avc444ReconstructionMode::Luma => {
                apply_luma_in_place(&mut candidate, luma, luma_regions)?;
                candidate_has_luma = true;
            }
            Avc444ReconstructionMode::LumaAndChroma(layout) => {
                let chroma = chroma.ok_or(Avc444ReconstructionError::InvalidPlane)?;
                if chroma.width != self.frame.width || chroma.height != self.frame.height {
                    return Err(Avc444ReconstructionError::InvalidDimensions);
                }
                apply_luma_in_place(&mut candidate, luma, luma_regions)?;
                apply_chroma_in_place(&mut candidate, chroma, chroma_regions, layout)?;
                candidate_has_luma = true;
            }
            Avc444ReconstructionMode::Chroma(layout) => {
                if !self.has_luma_reference {
                    return Err(Avc444ReconstructionError::MissingLumaReference);
                }
                let chroma = chroma.ok_or(Avc444ReconstructionError::InvalidPlane)?;
                if chroma.width != self.frame.width || chroma.height != self.frame.height {
                    return Err(Avc444ReconstructionError::InvalidDimensions);
                }
                apply_chroma_in_place(&mut candidate, chroma, chroma_regions, layout)?;
            }
        }
        self.frame = candidate;
        self.has_luma_reference = candidate_has_luma;
        Ok(())
    }
}

fn apply_luma_in_place(
    destination: &mut Yuv444Frame,
    source: &Yuv420Frame<'_>,
    regions: &[InclusiveRectangle],
) -> Result<(), Avc444ReconstructionError> {
    for region in regions {
        let (left, top, width, height) =
            checked_region(region, destination.width, destination.height)?;
        let half_width = width.div_ceil(2);
        let half_height = height.div_ceil(2);
        for y in 0..height {
            for x in 0..width {
                destination.y[(top + y) * destination.width + left + x] =
                    source.y.sample(left + x, top + y)?;
            }
        }
        for y in 0..half_height {
            for x in 0..half_width {
                let u = source.u.sample(left / 2 + x, top / 2 + y)?;
                let v = source.v.sample(left / 2 + x, top / 2 + y)?;
                for dy in 0..2 {
                    let dst_y = top + 2 * y + dy;
                    if dst_y >= top + height {
                        continue;
                    }
                    for dx in 0..2 {
                        let dst_x = left + 2 * x + dx;
                        if dst_x >= left + width {
                            continue;
                        }
                        let index = dst_y * destination.width + dst_x;
                        destination.u[index] = u;
                        destination.v[index] = v;
                    }
                }
            }
        }
    }
    Ok(())
}

fn apply_chroma_in_place(
    destination: &mut Yuv444Frame,
    source: &Yuv420Frame<'_>,
    regions: &[InclusiveRectangle],
    layout: Avc444ChromaLayout,
) -> Result<(), Avc444ReconstructionError> {
    for region in regions {
        match layout {
            Avc444ChromaLayout::V1 => apply_chroma_v1(destination, source, region)?,
            Avc444ChromaLayout::V2 => apply_chroma_v2(destination, source, region)?,
        }
    }
    Ok(())
}

fn apply_chroma_v1(
    destination: &mut Yuv444Frame,
    source: &Yuv420Frame<'_>,
    region: &InclusiveRectangle,
) -> Result<(), Avc444ReconstructionError> {
    let (left, top, width, height) = checked_region(region, destination.width, destination.height)?;
    let half_width = width.div_ceil(2);
    let half_height = height.div_ceil(2);
    let padded_height = height
        .checked_add(16 - height % 16)
        .ok_or(Avc444ReconstructionError::RegionOutOfBounds)?;

    let mut u_y = 0usize;
    let mut v_y = 0usize;
    for y in 0..padded_height {
        let (is_u, row) = if y % 16 < 8 {
            let row = 2 * u_y + 1;
            u_y = u_y.saturating_add(1);
            (true, row)
        } else {
            let row = 2 * v_y + 1;
            v_y = v_y.saturating_add(1);
            (false, row)
        };
        if row >= height || y >= height {
            continue;
        }
        for x in 0..width {
            let value = source.y.sample(left + x, top + y)?;
            let index = (top + row) * destination.width + left + x;
            if is_u {
                destination.u[index] = value;
            } else {
                destination.v[index] = value;
            }
        }
    }

    for y in 0..half_height {
        for x in 0..half_width {
            let u = source.u.sample(left / 2 + x, top / 2 + y)?;
            let v = source.v.sample(left / 2 + x, top / 2 + y)?;
            let dst_y = top + 2 * y;
            let dst_x = left + 2 * x + 1;
            if dst_y >= top + height || dst_x >= left + width {
                continue;
            }
            let index = dst_y * destination.width + dst_x;
            destination.u[index] = u;
            destination.v[index] = v;
        }
    }
    Ok(())
}

fn apply_chroma_v2(
    destination: &mut Yuv444Frame,
    source: &Yuv420Frame<'_>,
    region: &InclusiveRectangle,
) -> Result<(), Avc444ReconstructionError> {
    let (left, top, width, height) = checked_region(region, destination.width, destination.height)?;
    let half_width = width.div_ceil(2);
    let half_height = height.div_ceil(2);
    let quarter_width = (width + 3) / 4;
    let source_v_offset = source.width / 2;

    for y in 0..height {
        for x in 0..half_width {
            let u = source.y.sample(left / 2 + x, top + y)?;
            let v = source.y.sample(source_v_offset + left / 2 + x, top + y)?;
            let dst_x = left + 2 * x + 1;
            if dst_x >= left + width {
                continue;
            }
            let index = (top + y) * destination.width + dst_x;
            destination.u[index] = u;
            destination.v[index] = v;
        }
    }

    let source_chroma_v_offset = source.width / 4;
    for y in 0..half_height {
        let dst_y = top + 2 * y + 1;
        if dst_y >= top + height {
            continue;
        }
        for x in 0..quarter_width {
            let u0 = source.u.sample(left / 4 + x, top / 2 + y)?;
            let v0 = source.v.sample(left / 4 + x, top / 2 + y)?;
            let u1 = source
                .u
                .sample(source_chroma_v_offset + left / 4 + x, top / 2 + y)?;
            let v1 = source
                .v
                .sample(source_chroma_v_offset + left / 4 + x, top / 2 + y)?;
            let x0 = left + 4 * x;
            let x2 = x0 + 2;
            if x0 < left + width {
                let index = dst_y * destination.width + x0;
                destination.u[index] = u0;
                destination.v[index] = v0;
            }
            if x2 < left + width {
                let index = dst_y * destination.width + x2;
                destination.u[index] = u1;
                destination.v[index] = v1;
            }
        }
    }
    Ok(())
}

fn checked_region(
    region: &InclusiveRectangle,
    width: usize,
    height: usize,
) -> Result<(usize, usize, usize, usize), Avc444ReconstructionError> {
    let left = usize::from(region.left);
    let top = usize::from(region.top);
    let right = usize::from(region.right);
    let bottom = usize::from(region.bottom);
    if left > right || top > bottom || right >= width || bottom >= height {
        return Err(Avc444ReconstructionError::RegionOutOfBounds);
    }
    Ok((left, top, right - left + 1, bottom - top + 1))
}

#[cfg(test)]
mod tests {
    use super::{
        Avc444ChromaLayout, Avc444ReconstructionError, Avc444ReconstructionMode, Yuv420Frame,
        Yuv420Plane, Yuv444Reconstructor,
    };
    use ironrdp::pdu::geometry::InclusiveRectangle;

    fn yuv420_frame<'a>(
        width: usize,
        height: usize,
        y: &'a [u8],
        u: &'a [u8],
        v: &'a [u8],
    ) -> Yuv420Frame<'a> {
        Yuv420Frame::try_new(
            width,
            height,
            Yuv420Plane::new(y, width, width, height).unwrap(),
            Yuv420Plane::new(u, width.div_ceil(2), width.div_ceil(2), height.div_ceil(2)).unwrap(),
            Yuv420Plane::new(v, width.div_ceil(2), width.div_ceil(2), height.div_ceil(2)).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn luma_mode_expands_yuv420_chroma_to_yuv444() {
        let y = [10, 20, 30, 40];
        let u = [50];
        let v = [60];
        let source = yuv420_frame(2, 2, &y, &u, &v);
        let region = InclusiveRectangle {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        };
        let mut reconstructor = Yuv444Reconstructor::try_new(2, 2).unwrap();
        reconstructor
            .apply(
                Avc444ReconstructionMode::Luma,
                &source,
                &[region],
                None,
                &[],
            )
            .unwrap();
        let planes = reconstructor.frame().planes();
        assert_eq!(planes[0], &[10, 20, 30, 40]);
        assert_eq!(planes[1], &[50, 50, 50, 50]);
        assert_eq!(planes[2], &[60, 60, 60, 60]);
    }

    #[test]
    fn chroma_v1_requires_luma_and_updates_only_chroma_positions() {
        let y = vec![0; 16 * 16];
        let u = vec![0; 8 * 8];
        let v = vec![0; 8 * 8];
        let luma = yuv420_frame(16, 16, &y, &u, &v);
        let aux_y = (0..16)
            .flat_map(|row| std::iter::repeat_n(100 + row as u8, 16))
            .collect::<Vec<_>>();
        let aux_u = vec![150; 8 * 8];
        let aux_v = vec![200; 8 * 8];
        let chroma = yuv420_frame(16, 16, &aux_y, &aux_u, &aux_v);
        let region = InclusiveRectangle {
            left: 0,
            top: 0,
            right: 15,
            bottom: 15,
        };
        let mut reconstructor = Yuv444Reconstructor::try_new(16, 16).unwrap();
        assert_eq!(
            reconstructor
                .apply(
                    Avc444ReconstructionMode::Chroma(Avc444ChromaLayout::V1),
                    &luma,
                    &[],
                    Some(&chroma),
                    &[region.clone()],
                )
                .unwrap_err(),
            Avc444ReconstructionError::MissingLumaReference
        );
        reconstructor
            .apply(
                Avc444ReconstructionMode::Luma,
                &luma,
                &[region.clone()],
                None,
                &[],
            )
            .unwrap();
        reconstructor
            .apply(
                Avc444ReconstructionMode::Chroma(Avc444ChromaLayout::V1),
                &luma,
                &[],
                Some(&chroma),
                &[region],
            )
            .unwrap();
        let planes = reconstructor.frame().planes();
        assert_eq!(planes[1][0], 0);
        assert_eq!(planes[1][1], 150);
        assert_eq!(planes[1][2], 0);
        assert_eq!(planes[1][3], 150);
        assert_eq!(planes[1][16], 100);
        assert_eq!(planes[1][17], 100);
        assert_eq!(planes[1][49], 101);
        assert_eq!(planes[2][1], 200);
        assert_eq!(planes[2][17], 108);
    }

    #[test]
    fn invalid_region_does_not_commit_a_partial_frame() {
        let y = [1, 2, 3, 4];
        let u = [5];
        let v = [6];
        let source = yuv420_frame(2, 2, &y, &u, &v);
        let valid = InclusiveRectangle {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        };
        let invalid = InclusiveRectangle {
            left: 2,
            top: 0,
            right: 2,
            bottom: 0,
        };
        let mut reconstructor = Yuv444Reconstructor::try_new(2, 2).unwrap();
        assert_eq!(
            reconstructor
                .apply(
                    Avc444ReconstructionMode::Luma,
                    &source,
                    &[valid, invalid],
                    None,
                    &[],
                )
                .unwrap_err(),
            Avc444ReconstructionError::RegionOutOfBounds
        );
        assert_eq!(reconstructor.frame().planes()[0], &[0, 0, 0, 0]);
    }

    #[test]
    fn chroma_v2_expands_interleaved_aux_planes() {
        let luma_y = vec![0; 8 * 4];
        let luma_u = vec![0; 4 * 2];
        let luma_v = vec![0; 4 * 2];
        let luma = yuv420_frame(8, 4, &luma_y, &luma_u, &luma_v);

        let aux_y = (0..4)
            .flat_map(|row| {
                (0..8).map(move |column| {
                    if column < 4 {
                        10 + row as u8
                    } else {
                        20 + row as u8
                    }
                })
            })
            .collect::<Vec<_>>();
        let aux_u = [30, 31, 40, 41, 50, 51, 60, 61];
        let aux_v = [70, 71, 80, 81, 90, 91, 100, 101];
        let chroma = yuv420_frame(8, 4, &aux_y, &aux_u, &aux_v);
        let region = InclusiveRectangle {
            left: 0,
            top: 0,
            right: 7,
            bottom: 3,
        };

        let mut reconstructor = Yuv444Reconstructor::try_new(8, 4).unwrap();
        reconstructor
            .apply(
                Avc444ReconstructionMode::Luma,
                &luma,
                &[region.clone()],
                None,
                &[],
            )
            .unwrap();
        reconstructor
            .apply(
                Avc444ReconstructionMode::Chroma(Avc444ChromaLayout::V2),
                &luma,
                &[],
                Some(&chroma),
                &[region],
            )
            .unwrap();

        let planes = reconstructor.frame().planes();
        // The auxiliary luma half carries the odd-column U/V samples on every row.
        assert_eq!(planes[1][1], 10);
        assert_eq!(planes[2][1], 20);
        assert_eq!(planes[1][3], 10);
        assert_eq!(planes[2][3], 20);
        assert_eq!(planes[1][8 + 1], 11);
        assert_eq!(planes[2][8 + 1], 21);
        // The auxiliary chroma halves carry even-column samples on odd rows.
        assert_eq!(planes[1][8], 30);
        assert_eq!(planes[2][8], 70);
        assert_eq!(planes[1][10], 40);
        assert_eq!(planes[2][10], 80);
        assert_eq!(planes[1][24], 50);
        assert_eq!(planes[2][24], 90);
        assert_eq!(planes[1][26], 60);
        assert_eq!(planes[2][26], 100);
    }
}
