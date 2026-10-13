use frd_protocol_api::ProtocolError;
use ironrdp::graphics::legacy_native::{KernelError, NativeKernels};

use crate::error::{rdp_error, RDP_LEGACY_NATIVE_UNAVAILABLE};

/// Legacy 解码请求；不代表完整 codec、目标性能、包或互操作资格已通过。
/// 该策略独立于 wire 平台身份和 EGFX 广告门，不自动选择后端。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RdpLegacyDecodePolicy {
    /// 保留既有解码路径；不探测原生 CPU 能力。
    #[default]
    Compatibility,
    /// 必须使用当前进程实际 ISA 的 checked native 后端；不可用即停止。
    RequiredNative,
}

/// 只保存一次实际探测结果；会话中不重新探测、不重试 scalar。
#[derive(Clone, Copy)]
pub(crate) enum ResolvedLegacyDecode {
    Compatibility,
    CheckedNative(NativeKernels),
}

impl ResolvedLegacyDecode {
    pub(crate) fn resolve(policy: RdpLegacyDecodePolicy) -> Result<Self, ProtocolError> {
        Self::resolve_with(policy, NativeKernels::detect)
    }

    // 唯一生产入口固定为 detect；可替换探测入口只编译到测试。
    #[cfg(test)]
    pub(crate) fn resolve_with_probe(
        policy: RdpLegacyDecodePolicy,
        probe: fn() -> Result<NativeKernels, KernelError>,
    ) -> Result<Self, ProtocolError> {
        Self::resolve_with(policy, probe)
    }

    fn resolve_with(
        policy: RdpLegacyDecodePolicy,
        probe: impl FnOnce() -> Result<NativeKernels, KernelError>,
    ) -> Result<Self, ProtocolError> {
        match policy {
            RdpLegacyDecodePolicy::Compatibility => Ok(Self::Compatibility),
            RdpLegacyDecodePolicy::RequiredNative => probe()
                .map(Self::CheckedNative)
                .map_err(|_| rdp_error(RDP_LEGACY_NATIVE_UNAVAILABLE)),
        }
    }
}
