//! 普通应用平台组合根；默认不可用，显式夹具保持原行为。
use crate::app_adapter::{AppEngine, Stores};
use frd_protocol_api::ProtocolFactory;
use frd_shell_runtime::{AudioOutputFactory, StreamRegistryLoader};
use std::{path::PathBuf, sync::Arc};

struct NoAudio;
impl AudioOutputFactory for NoAudio {
    fn open(&self) -> Result<Box<dyn frd_media_api::AudioOutput>, frd_media_api::AudioOutputError> {
        Err(frd_media_api::AudioOutputError::Unavailable)
    }
}
fn factories_for_platform(_ohos_arm64: bool) -> Vec<Arc<dyn ProtocolFactory>> {
    #[cfg(feature = "normal-hap-rdp-validation")]
    if _ohos_arm64 {
        // new 保持 LegacyOnly 且没有 EGFX provider；原生核缺失由既有 factory 拒绝。
        return vec![Arc::new(
            frd_protocol_rdp::RdpProtocolFactory::new(
                frd_protocol_rdp::RdpClientPlatformIdentity::Unix,
            )
            .with_legacy_decode_policy(frd_protocol_rdp::RdpLegacyDecodePolicy::RequiredNative),
        )];
    }
    #[cfg(all(debug_assertions, feature = "native-presentation-fixture"))]
    {
        vec![Arc::new(crate::fixture::Factory)]
    }
    #[cfg(not(all(debug_assertions, feature = "native-presentation-fixture")))]
    {
        vec![]
    }
}
fn video_loader() -> StreamRegistryLoader {
    Arc::new(|_| {
        Err(frd_media_api::VideoDecodeError::new(
            frd_media_api::VideoDecodeErrorCode::BackendUnavailable,
        ))
    })
}
pub(crate) fn spawn(root: PathBuf) -> Result<AppEngine, i32> {
    AppEngine::spawn(
        Stores::at(root),
        factories_for_platform(cfg!(all(target_env = "ohos", target_arch = "aarch64"))),
        video_loader(),
        Arc::new(NoAudio),
    )
}
#[cfg(test)]
#[path = "composition_tests.rs"]
mod tests;
