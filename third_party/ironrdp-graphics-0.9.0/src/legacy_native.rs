//! 显式受检查的 legacy 消费者；调用方传入已验证 ISA 的能力，默认路径保持原版。
use crate::rlgr::{self, RlgrError};
use frd_rdp_legacy_kernels::Quantization;
pub use frd_rdp_legacy_kernels::{KernelError, NativeKernels, PackedDepth, Pixel32Format, PixelRowSource};
use ironrdp_pdu::codecs::rfx::{EntropyAlgorithm, Quant};

#[derive(Debug)]
pub enum NativeRfxError {
    Rlgr(RlgrError),
    Kernel(KernelError),
}
impl core::fmt::Display for NativeRfxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Rlgr(e) => e.fmt(f),
            Self::Kernel(e) => e.fmt(f),
        }
    }
}
impl core::error::Error for NativeRfxError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Rlgr(e) => Some(e),
            Self::Kernel(e) => Some(e),
        }
    }
}

/// 保留原版 RLGR wire/error/padding；仅在解析零游程和终端补齐时调用原生 zero。
pub fn decode_rlgr_native(
    mode: EntropyAlgorithm,
    tile: &[u8],
    output: &mut [i16],
    kernels: &NativeKernels,
) -> Result<(), RlgrError> {
    rlgr::decode_with(mode, tile, output, &mut rlgr::NativeZero::new(kernels))
}

#[cfg(test)]
std::thread_local! {
    pub(crate) static COMPONENT_CALLS: std::cell::RefCell<Vec<(&'static str, usize)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// 组件私有 scratch 允许部分更新；尺寸在首次写入前检查，错误由 session 阻止 tile 发布。
pub fn decode_component_native(
    quant: &Quant,
    mode: EntropyAlgorithm,
    data: &[u8],
    output: &mut [i16],
    scratch: &mut [i16],
    kernels: &NativeKernels,
) -> Result<(), NativeRfxError> {
    if output.len() != 4096 || scratch.len() != 4096 {
        return Err(NativeRfxError::Kernel(KernelError::Length));
    }
    decode_rlgr_native(mode, data, output, kernels).map_err(NativeRfxError::Rlgr)?;
    #[cfg(test)]
    COMPONENT_CALLS.with(|v| v.borrow_mut().push(("rlgr", output.len())));
    kernels.differential(&mut output[4032..4096]);
    #[cfg(test)]
    COMPONENT_CALLS.with(|v| v.borrow_mut().push(("differential", 64)));
    kernels
        .dequantize(
            output,
            Quantization([
                quant.hl1, quant.lh1, quant.hh1, quant.hl2, quant.lh2, quant.hh2, quant.hl3, quant.lh3, quant.hh3,
                quant.ll3,
            ]),
        )
        .map_err(NativeRfxError::Kernel)?;
    #[cfg(test)]
    COMPONENT_CALLS.with(|v| v.borrow_mut().push(("dequantize", output.len())));
    kernels.inverse_dwt(output, scratch).map_err(NativeRfxError::Kernel)?;
    #[cfg(test)]
    COMPONENT_CALLS.with(|v| v.borrow_mut().push(("inverse_dwt", output.len())));
    Ok(())
}
