//! Progressive 的有界会话状态；wire、熵解码和 SIMD 数值内核分别接入。
//! 依据 MS-RDPEGFX 2.2.4.2 与 3.3.8.2，不调用 IronRDP scalar 高层 decoder。

mod entropy;
mod kernels;
mod native;
mod state;
#[cfg(test)]
mod tests;
mod wire;
pub use state::*;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    MissingTile,
    ResourceLimit,
    BackendUnavailable,
    Backend(&'static str),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Progressive: {self:?}")
    }
}
impl std::error::Error for Error {}

pub(crate) type NativeDecoder = Decoder<native::NativeBackend>;
pub(crate) fn native_decoder() -> Option<NativeDecoder> {
    Some(Decoder::new(
        native::NativeBackend::new()?,
        Limits::default(),
    ))
}
pub(crate) fn decode_wire(
    data: &[u8],
) -> Result<Vec<ironrdp::pdu::codecs::rfx::progressive::ProgressiveBlock<'_>>> {
    wire::decode(data).map_err(|_| Error::Invalid("wire envelope"))
}
