//! FFmpeg 8.1.2 客户端解码插件的稳定 C ABI 入口。
//!
//! 默认构建不链接 FFmpeg，并以空 API 指针失败关闭。Task 5 提供固定 LGPL 动态库后，必须
//! 显式启用 `native-ffmpeg`，且只有 native decoder 完整可用时才能返回 ABI 表。

mod decoder;

use frd_video_ffmpeg::abi::FrdFfmpegApiV1;

#[no_mangle]
pub extern "C" fn frd_ffmpeg_get_api_v1() -> *const FrdFfmpegApiV1 {
    decoder::api()
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(not(feature = "native-ffmpeg"))]
    fn plugin_without_native_ffmpeg_reports_unavailable() {
        assert!(super::frd_ffmpeg_get_api_v1().is_null());
    }
}
