//! Linux 外部上下文 GL 执行器。DrawReceipt 仅证明命令提交，无生产呈现 ACK。
#[cfg(any(
    test,
    all(
        target_os = "linux",
        feature = "linux-gl",
        any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
    )
))]
use frd_core::{PixelRect, PixelSize};
use frd_render_state::{FrameBatchIdentity, RecoveryRequirement, TransactionError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlError {
    Unavailable,
    ContextNotCurrent,
    ContextLost,
    UnsupportedContext,
    InvalidTarget,
    InvalidViewport,
    ResourceCreation,
    ShaderCompilation,
    GlCommand,
    ResetRequired,
    Transaction(TransactionError),
}
impl From<TransactionError> for GlError {
    fn from(value: TransactionError) -> Self {
        Self::Transaction(value)
    }
}
#[derive(Debug)]
pub struct GlBatchFailure {
    pub identity: Option<FrameBatchIdentity>,
    pub error: GlError,
    pub recovery: Option<RecoveryRequirement>,
}

pub const fn available() -> bool {
    cfg!(all(
        target_os = "linux",
        feature = "linux-gl",
        any(
            target_arch = "x86",
            target_arch = "x86_64",
            target_arch = "aarch64"
        )
    ))
}

/// 输入为窗口顶部原点，GL viewport 使用底部原点。
#[cfg(any(
    test,
    all(
        target_os = "linux",
        feature = "linux-gl",
        any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
    )
))]
fn gl_viewport(target: PixelSize, content: PixelRect) -> Result<[i32; 4], GlError> {
    let (_, end) = content.checked_bounds().ok_or(GlError::InvalidViewport)?;
    if end.x > target.width || end.y > target.height {
        return Err(GlError::InvalidViewport);
    }
    let values = [
        content.x,
        target.height - end.y,
        content.width,
        content.height,
    ];
    let mut out = [0; 4];
    for (destination, value) in out.iter_mut().zip(values) {
        *destination = i32::try_from(value).map_err(|_| GlError::InvalidViewport)?;
    }
    Ok(out)
}

#[cfg(all(
    target_os = "linux",
    feature = "linux-gl",
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
mod backend;
#[cfg(all(
    target_os = "linux",
    feature = "linux-gl",
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
pub use backend::{
    DrawReceipt, ExternalContext, GlOutputContract, GlRenderTarget, RemoteGlRenderer,
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn viewport_uses_bottom_origin_and_rejects_overflow() {
        let target = PixelSize::new(100, 80).unwrap();
        assert_eq!(
            gl_viewport(
                target,
                PixelRect {
                    x: 3,
                    y: 7,
                    width: 90,
                    height: 60
                }
            ),
            Ok([3, 13, 90, 60])
        );
        for rect in [
            PixelRect {
                x: 3,
                y: 7,
                width: 100,
                height: 60,
            },
            PixelRect {
                x: u32::MAX,
                y: 0,
                width: 2,
                height: 1,
            },
        ] {
            assert_eq!(gl_viewport(target, rect), Err(GlError::InvalidViewport));
        }
    }
    #[test]
    fn platform_gate_is_explicit() {
        assert_eq!(
            available(),
            cfg!(all(
                target_os = "linux",
                feature = "linux-gl",
                any(
                    target_arch = "x86",
                    target_arch = "x86_64",
                    target_arch = "aarch64"
                )
            ))
        );
    }
}

#[cfg(any(
    test,
    all(
        target_os = "linux",
        feature = "linux-gl",
        any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
    )
))]
fn next_draw_serial(serial: u64) -> Result<u64, GlError> {
    serial.checked_add(1).ok_or(GlError::ContextLost)
}
#[cfg(test)]
mod lifecycle_tests {
    #[test]
    fn proof_serial_never_reuses_an_exhausted_identity() {
        assert_eq!(super::next_draw_serial(0), Ok(1));
        assert_eq!(super::next_draw_serial(u64::MAX - 1), Ok(u64::MAX));
        assert_eq!(
            super::next_draw_serial(u64::MAX),
            Err(super::GlError::ContextLost)
        );
    }
}
