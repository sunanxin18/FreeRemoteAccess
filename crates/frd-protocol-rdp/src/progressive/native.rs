//! Strict entropy 与架构 SIMD 的 tile 桥；任何失败不改变上一 tile。
use super::entropy::{decode_rlgr1, UpgradeReader};
use super::kernels::{BandLayout, NativeKernels};
use super::{Error, Result, TileDecoder, TileParameters, TileRequest};
use ironrdp::pdu::codecs::rfx::progressive::ProgressiveTile;

#[derive(Clone)]
pub(crate) struct NativeReference {
    current: Box<[[i16; 4096]; 3]>,
}
#[derive(Clone)]
pub(crate) struct NativeTileState {
    das: Box<[[i16; 4096]; 3]>,
}
pub(crate) struct NativeBackend {
    kernels: NativeKernels,
}
impl NativeBackend {
    pub(super) fn new() -> Option<Self> {
        Some(Self {
            kernels: NativeKernels::new()?,
        })
    }
}
fn entropy_error(error: super::entropy::Error) -> Error {
    Error::Backend(match error {
        super::entropy::Error::Truncated => "entropy truncated",
        super::entropy::Error::TrailingData => "entropy trailing data",
        super::entropy::Error::InvalidBits => "entropy invalid bit count",
        super::entropy::Error::InvalidSign => "entropy invalid sign",
        super::entropy::Error::RunOverrun => "entropy run overrun",
        super::entropy::Error::CoefficientRange => "entropy coefficient range",
        super::entropy::Error::TooManyCoefficients => "entropy coefficient budget",
    })
}
fn positions(parameters: &TileParameters, component: usize) -> Result<[u8; 10]> {
    let mut positions = [0; 10];
    for (band, position) in positions.iter_mut().enumerate() {
        *position = parameters.base[component]
            .for_band(band)
            .checked_add(parameters.progressive[component].for_band(band))
            .filter(|p| (1..=23).contains(p))
            .ok_or(Error::Invalid("coefficient bit position"))?;
    }
    Ok(positions)
}
fn tail(data: &[u8]) -> Result<()> {
    // MS-RDPEGFX Appendix A 的旧 RDP 8.0 tail 兼容序列；其他 tail 只接受零。
    if data.iter().all(|v| *v == 0) || data == [0x4c, 0x41, 1, 0, 0xff, 0xff, 0, 0x10] {
        Ok(())
    } else {
        Err(Error::Invalid("tile tail"))
    }
}
impl TileDecoder for NativeBackend {
    type TileState = NativeTileState;
    type ReferenceState = NativeReference;
    const STATE_BYTES: usize = 3 * 4096 * 2 + std::mem::size_of::<NativeTileState>();
    const REFERENCE_BYTES: usize = 3 * 4096 * 2 + std::mem::size_of::<NativeReference>();
    const WORKSPACE_BYTES: usize = 128 * 1024;
    fn decode_tile(
        &self,
        reference: Option<&NativeReference>,
        previous: Option<&NativeTileState>,
        request: TileRequest<'_>,
    ) -> Result<(NativeReference, NativeTileState, Vec<u8>)> {
        let layout = if request.parameters.reduce_extrapolate {
            BandLayout::ReduceExtrapolate
        } else {
            BandLayout::Normal
        };
        let mut current = reference.cloned().unwrap_or_else(|| NativeReference {
            current: Box::new([[0; 4096]; 3]),
        });
        let mut next = match previous {
            Some(previous) => previous.clone(),
            None => NativeTileState {
                das: Box::new([[0; 4096]; 3]),
            },
        };
        match request.tile {
            ProgressiveTile::Simple(t) => {
                tail(t.tail_data)?;
                self.first(
                    &mut current,
                    &mut next,
                    [t.y_data, t.cb_data, t.cr_data],
                    request.parameters,
                    reference.is_some(),
                    layout,
                )?;
            }
            ProgressiveTile::First(t) => {
                tail(t.tail_data)?;
                self.first(
                    &mut current,
                    &mut next,
                    [t.y_data, t.cb_data, t.cr_data],
                    request.parameters,
                    reference.is_some(),
                    layout,
                )?;
            }
            ProgressiveTile::Upgrade(t) => {
                let prior = request.previous_parameters.ok_or(Error::MissingTile)?;
                if previous.is_none() || reference.is_none() {
                    return Err(Error::MissingTile);
                }
                let srl = [t.y_srl_data, t.cb_srl_data, t.cr_srl_data];
                let raw = [t.y_raw_data, t.cb_raw_data, t.cr_raw_data];
                for component in 0..3 {
                    let old = positions(prior, component)?;
                    let new = positions(request.parameters, component)?;
                    let shifts = new.map(|n| n - 1);
                    let mut reader = UpgradeReader::new(srl[component], raw[component]);
                    let mut magnitudes = Box::new([0u16; 4096]);
                    let mut negative = Box::new([false; 4096]);
                    let mut offset = 0;
                    for (band, len) in layout.lengths().into_iter().enumerate() {
                        let bits = old[band]
                            .checked_sub(new[band])
                            .ok_or(Error::Invalid("refinement regressed"))?;
                        if bits != 0 {
                            let values = reader
                                .read_band(
                                    &next.das[component][offset..offset + len],
                                    bits,
                                    band == 9,
                                )
                                .map_err(entropy_error)?;
                            // 仅解包协议字段；符号/shift/累加全部由 SIMD leaf 执行。
                            for (index, value) in values.into_iter().enumerate() {
                                magnitudes[offset + index] = value.magnitude;
                                negative[offset + index] = value.negative;
                            }
                        }
                        offset += len;
                    }
                    reader.finish().map_err(entropy_error)?;
                    self.kernels
                        .apply_refinement(
                            &mut current.current[component],
                            &mut next.das[component],
                            &magnitudes,
                            &negative,
                            &shifts,
                            layout,
                        )
                        .map_err(|_| Error::Backend("refinement kernel"))?;
                }
            }
        }
        let mut render = current.current.clone();
        let mut scratch = Box::new([0i16; 4096]);
        for plane in render.iter_mut() {
            self.kernels
                .inverse_dwt(plane, &mut scratch, layout)
                .map_err(|_| Error::Backend("inverse DWT kernel"))?;
        }
        let mut pixels = Box::new([0u8; 16384]);
        self.kernels
            .ycbcr_to_bgra(&render[0], &render[1], &render[2], &mut pixels);
        Ok((current, next, pixels.to_vec()))
    }
}
impl NativeBackend {
    fn first(
        &self,
        current: &mut NativeReference,
        next: &mut NativeTileState,
        data: [&[u8]; 3],
        parameters: &TileParameters,
        has_previous: bool,
        layout: BandLayout,
    ) -> Result<()> {
        if parameters.difference && !has_previous {
            return Err(Error::MissingTile);
        }
        for (component, data) in data.into_iter().enumerate() {
            let shifts = positions(parameters, component)?.map(|p| p - 1);
            let mut coefficients = Box::new([0i16; 4096]);
            decode_rlgr1(data, &mut coefficients[..]).map_err(entropy_error)?;
            self.kernels
                .capture_sign(&coefficients, &mut next.das[component]);
            self.kernels.prefix_sum_ll3(&mut coefficients, layout);
            self.kernels
                .shift_bands(&mut coefficients, &shifts, layout)
                .map_err(|_| Error::Backend("dequantization kernel"))?;
            if parameters.difference {
                self.kernels
                    .add(&mut current.current[component], &coefficients);
            } else {
                current.current[component].copy_from_slice(&coefficients[..]);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
