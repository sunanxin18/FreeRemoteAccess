//! FFmpeg 8.1.2 客户端解码插件的稳定 C ABI 入口。
//!
//! 默认构建不链接 FFmpeg，并以空 API 指针失败关闭。只有使用固定 LGPL 动态库显式启用
//! `native-ffmpeg`，且 native decoder 完整可用时，插件才返回 ABI 表。

mod decoder;

use frd_video_ffmpeg::abi::{FrdStatus, RawFrdFfmpegApiV1};

#[no_mangle]
pub extern "C" fn frd_ffmpeg_get_api_v1(
    output: *mut RawFrdFfmpegApiV1,
    output_size: usize,
) -> FrdStatus {
    decoder::populate_api(output, output_size)
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(not(feature = "native-ffmpeg"))]
    fn plugin_without_native_ffmpeg_reports_unavailable() {
        let mut output = frd_video_ffmpeg::abi::RawFrdFfmpegApiV1::default();

        let status = super::frd_ffmpeg_get_api_v1(
            &mut output,
            std::mem::size_of::<frd_video_ffmpeg::abi::RawFrdFfmpegApiV1>(),
        );

        assert_eq!(status, frd_video_ffmpeg::abi::FrdStatus::UNSUPPORTED);
        assert_eq!(output, frd_video_ffmpeg::abi::RawFrdFfmpegApiV1::default());
    }
}
