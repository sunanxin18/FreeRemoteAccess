use frd_video_ffmpeg::abi::{FrdStatus, RawFrdFfmpegApiV1};

pub(crate) fn populate_api(output: *mut RawFrdFfmpegApiV1, output_size: usize) -> FrdStatus {
    if output.is_null() || output_size < std::mem::size_of::<RawFrdFfmpegApiV1>() {
        return FrdStatus::INVALID_ARGUMENT;
    }
    #[cfg(feature = "native-ffmpeg")]
    {
        // The pinned native dependency is intentionally queried here, but Task 4 has no decoder
        // implementation that can satisfy `DecoderReady`. Keep the export unavailable until Task
        // 5 validates YUV444P output and supplies the complete function table.
        let _native_prerequisites_present = native_prerequisites_present();
    }

    FrdStatus::UNSUPPORTED
}

#[cfg(feature = "native-ffmpeg")]
fn native_prerequisites_present() -> bool {
    use ffmpeg_the_third::{codec, ffi};
    use frd_video_ffmpeg::abi::FRD_FFMPEG_AVCODEC_MAJOR;

    // SAFETY: this function is reached only after the dynamic linker has resolved the pinned
    // FFmpeg build. Both calls are read-only library queries and return library-owned pointers.
    let avcodec_major = unsafe { ffi::avcodec_version() >> 16 };
    avcodec_major == FRD_FFMPEG_AVCODEC_MAJOR && codec::decoder::find(codec::Id::HEVC).is_some()
}
