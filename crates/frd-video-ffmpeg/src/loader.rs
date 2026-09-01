use std::fmt;
use std::mem::MaybeUninit;
use std::path::{Component, Path, PathBuf};
use std::ptr;
use std::slice;
use std::sync::Arc;

use frd_core::PixelSize;
use frd_media_api::{
    ChromaFormat, DecodeOutcome, DecodedVideoFrame, DecodedVideoFrameInput, EncodedVideoAccessUnit,
    VideoBackendAvailability, VideoBackendId, VideoBackendKind, VideoBitstreamFormat,
    VideoCapabilityProvider, VideoCodec, VideoDecodeCapability, VideoDecodeError,
    VideoDecodeErrorCode, VideoDecodeQuery, VideoDecodeSupport, VideoDecoder, VideoDecoderFactory,
    VideoPixelFormat, VideoPlane, VideoProfile, VideoStreamConfig, VideoTimestamp,
    VideoUnsupportedReason, MAX_DECODED_VIDEO_FRAME_BYTES,
};
use libloading::{Library, Symbol};

pub use crate::abi::FRD_FFMPEG_ABI_VERSION;
use crate::abi::{
    FrdByteSlice, FrdDecodedFrame, FrdDecoderHandle, FrdFfmpegApiV1, FrdGetFfmpegApiV1,
    FrdOwnedBuffer, FrdStatus, FrdVideoConfig, FRD_BITSTREAM_ANNEX_B, FRD_CHROMA_YUV_444,
    FRD_CODEC_HEVC, FRD_FFMPEG_API_SYMBOL, FRD_FFMPEG_AVCODEC_MAJOR, FRD_PIXEL_FORMAT_YUV_444_P8,
    FRD_PROFILE_HEVC_MAIN_444_8, FRD_SUBMIT_RANDOM_ACCESS,
};

/// 已通过固定路径、ABI 和 libavcodec 版本门禁的可选 FFmpeg backend。
pub struct FfmpegBackend {
    plugin: Arc<LoadedPlugin>,
}

struct LoadedPlugin {
    api: FrdFfmpegApiV1,
    _libraries: Vec<Library>,
}

impl fmt::Debug for FfmpegBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegBackend")
            .field("abi_version", &self.plugin.api.abi_version)
            .field("avcodec_major", &self.plugin.api.avcodec_major)
            .finish_non_exhaustive()
    }
}

impl FfmpegBackend {
    /// 只从应用目录下固定版本、固定平台子目录加载；失败只禁用该 backend。
    pub fn load_from(application_dir: impl AsRef<Path>) -> Result<Self, VideoDecodeError> {
        let codec_dir = canonical_codec_dir(application_dir.as_ref())?;
        let mut libraries = Vec::new();

        for name in ffmpeg_dependency_names() {
            let path = canonical_library_path(&codec_dir, name)?;
            libraries.push(open_library(&path)?);
        }

        let plugin_path = canonical_library_path(&codec_dir, plugin_library_name())?;
        let plugin = open_library(&plugin_path)?;
        let api = load_api(&plugin)?;
        libraries.push(plugin);

        Self::from_api(api, libraries)
    }

    fn from_api(api: FrdFfmpegApiV1, libraries: Vec<Library>) -> Result<Self, VideoDecodeError> {
        if api.abi_version != FRD_FFMPEG_ABI_VERSION
            || api.avcodec_major != FRD_FFMPEG_AVCODEC_MAJOR
        {
            return Err(VideoDecodeError::new(
                VideoDecodeErrorCode::BackendVersionMismatch,
            ));
        }
        Ok(Self {
            plugin: Arc::new(LoadedPlugin {
                api,
                _libraries: libraries,
            }),
        })
    }

    #[cfg(test)]
    fn from_api_for_test(api: FrdFfmpegApiV1) -> Result<Self, VideoDecodeError> {
        Self::from_api(api, Vec::new())
    }
}

impl VideoCapabilityProvider for FfmpegBackend {
    fn backend_id(&self) -> VideoBackendId {
        VideoBackendId::new("ffmpeg-8.1.2-software")
    }

    fn backend_kind(&self) -> VideoBackendKind {
        VideoBackendKind::Ffmpeg
    }

    fn availability(&self) -> VideoBackendAvailability {
        VideoBackendAvailability::DecoderReady
    }

    fn query(&self, query: &VideoDecodeQuery) -> VideoDecodeSupport {
        if query.codec != VideoCodec::Hevc {
            return VideoDecodeSupport::Unsupported(VideoUnsupportedReason::CodecUnavailable);
        }
        if query.profile != VideoProfile::HevcMain4448 {
            return VideoDecodeSupport::Unsupported(VideoUnsupportedReason::ProfileUnavailable);
        }
        if query.chroma != ChromaFormat::Yuv444 {
            return VideoDecodeSupport::Unsupported(VideoUnsupportedReason::ChromaUnavailable);
        }
        if query.bit_depth != 8 {
            return VideoDecodeSupport::Unsupported(VideoUnsupportedReason::BitDepthUnavailable);
        }
        let maximum = PixelSize::new(8192, 8192).expect("固定 FFmpeg 尺寸上限有效");
        if query.coded_size.width > maximum.width || query.coded_size.height > maximum.height {
            return VideoDecodeSupport::Unsupported(VideoUnsupportedReason::DimensionsUnavailable);
        }
        if !query
            .preferred_outputs
            .contains(&VideoPixelFormat::Yuv444P8)
        {
            return VideoDecodeSupport::Unsupported(
                VideoUnsupportedReason::OutputFormatUnavailable,
            );
        }

        VideoDecodeSupport::SoftwareExact(VideoDecodeCapability {
            backend_id: self.backend_id(),
            codec: VideoCodec::Hevc,
            profile: VideoProfile::HevcMain4448,
            chroma: ChromaFormat::Yuv444,
            bit_depth: 8,
            max_coded_size: maximum,
            output_formats: vec![VideoPixelFormat::Yuv444P8].into_boxed_slice(),
            requires_bitstream_conversion: false,
        })
    }
}

impl VideoDecoderFactory for FfmpegBackend {
    fn create(
        &self,
        config: &VideoStreamConfig,
    ) -> Result<Box<dyn VideoDecoder>, VideoDecodeError> {
        let input = config.as_input();
        let query = VideoDecodeQuery {
            codec: input.codec,
            profile: input.profile,
            chroma: input.chroma,
            bit_depth: input.bit_depth,
            coded_size: input.coded_size,
            frame_rate: None,
            preferred_outputs: vec![VideoPixelFormat::Yuv444P8].into_boxed_slice(),
        };
        if !self.query(&query).is_exact() || input.bitstream_format != VideoBitstreamFormat::AnnexB
        {
            return Err(VideoDecodeError::new(
                VideoDecodeErrorCode::ExactProfileChromaBitDepthUnsupported,
            ));
        }

        FfmpegDecoder::create(Arc::clone(&self.plugin), config.clone())
            .map(|decoder| Box::new(decoder) as Box<dyn VideoDecoder>)
    }
}

struct FfmpegDecoder {
    plugin: Arc<LoadedPlugin>,
    handle: SendDecoderHandle,
    config: VideoStreamConfig,
    emitted_frame: bool,
}

/// The plugin ABI gives exclusive ownership of a decoder handle to the caller and permits moving
/// that ownership to the dedicated decoder worker. Calls remain serialized through `&mut self`.
struct SendDecoderHandle(FrdDecoderHandle);

// SAFETY: the versioned plugin contract requires an exclusively owned handle to be movable but
// never concurrently callable. `FfmpegDecoder` exposes it only through `&mut self` and destroys it
// exactly once while the originating dynamic library remains loaded.
unsafe impl Send for SendDecoderHandle {}

impl FfmpegDecoder {
    fn create(
        plugin: Arc<LoadedPlugin>,
        config: VideoStreamConfig,
    ) -> Result<Self, VideoDecodeError> {
        let handle = create_decoder_handle(plugin.api, &config)?;
        Ok(Self {
            plugin,
            handle,
            config,
            emitted_frame: false,
        })
    }

    fn receive_frames(&mut self) -> Result<Box<[DecodedVideoFrame]>, VideoDecodeError> {
        const MAX_FRAMES_PER_CALL: usize = 64;

        let mut frames = Vec::new();
        for _ in 0..MAX_FRAMES_PER_CALL {
            let mut raw_frame = MaybeUninit::<FrdDecodedFrame>::uninit();
            // SAFETY: the handle is exclusively owned, the plugin is retained, and `receive`
            // initializes the complete output only when it returns `OK`.
            let status =
                unsafe { (self.plugin.api.receive)(self.handle.0, raw_frame.as_mut_ptr()) };
            if status == FrdStatus::NEED_MORE_DATA || status == FrdStatus::END_OF_STREAM {
                return Ok(frames.into_boxed_slice());
            }
            if status != FrdStatus::OK {
                return Err(self.decode_error());
            }
            // SAFETY: `OK` is the ABI guarantee that every field of `FrdDecodedFrame` was written.
            let raw_frame = unsafe { raw_frame.assume_init() };
            let frame = convert_frame(&self.config, raw_frame)?;
            self.emitted_frame = true;
            frames.push(frame);
        }

        Err(self.decode_error())
    }

    fn decode_error(&self) -> VideoDecodeError {
        VideoDecodeError::new(if self.emitted_frame {
            VideoDecodeErrorCode::DecodeFailedAfterFirstFrame
        } else {
            VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame
        })
    }
}

impl VideoDecoder for FfmpegDecoder {
    fn submit(
        &mut self,
        access_unit: EncodedVideoAccessUnit,
    ) -> Result<DecodeOutcome, VideoDecodeError> {
        let input = self.config.as_input();
        if access_unit.identity() != input.identity || access_unit.generation() != input.generation
        {
            return Err(VideoDecodeError::new(
                VideoDecodeErrorCode::StaleStreamOrGeneration,
            ));
        }
        let timestamp = i64::try_from(access_unit.timestamp().ticks).map_err(|_| {
            VideoDecodeError::new(VideoDecodeErrorCode::MalformedOrOverBudgetAccessUnit)
        })?;
        let flags = if access_unit.random_access() {
            FRD_SUBMIT_RANDOM_ACCESS
        } else {
            0
        };
        // SAFETY: the input slice is valid for the duration of the call, its size was bounded by
        // `EncodedVideoAccessUnit`, and the exclusive handle cannot be called concurrently.
        let status = unsafe {
            (self.plugin.api.submit)(
                self.handle.0,
                access_unit.bytes().as_ptr(),
                access_unit.bytes().len(),
                timestamp,
                flags,
            )
        };
        if status != FrdStatus::OK && status != FrdStatus::NEED_MORE_DATA {
            return Err(self.decode_error());
        }
        let frames = self.receive_frames()?;
        if frames.is_empty() {
            Ok(DecodeOutcome::NeedMoreData)
        } else {
            Ok(DecodeOutcome::Frames(frames))
        }
    }

    fn flush(&mut self) -> Result<Box<[DecodedVideoFrame]>, VideoDecodeError> {
        // SAFETY: the handle is exclusively owned and the plugin library remains loaded.
        let status = unsafe { (self.plugin.api.flush)(self.handle.0) };
        if status != FrdStatus::OK && status != FrdStatus::END_OF_STREAM {
            return Err(self.decode_error());
        }
        self.receive_frames()
    }

    fn reset(&mut self, generation: u64) -> Result<(), VideoDecodeError> {
        let mut input = self.config.as_input().clone();
        input.generation = generation;
        let next_config = VideoStreamConfig::try_new(input)
            .map_err(|_| VideoDecodeError::new(VideoDecodeErrorCode::DecoderCreationFailed))?;
        let next_handle = create_decoder_handle(self.plugin.api, &next_config)?;

        // SAFETY: `next_handle` was created before releasing the old exclusive handle, so failure
        // cannot leave the decoder without a valid handle. The old handle is destroyed once.
        unsafe { (self.plugin.api.destroy)(self.handle.0) };
        self.handle = next_handle;
        self.config = next_config;
        self.emitted_frame = false;
        Ok(())
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        // SAFETY: this is the only owner of the non-null handle and `plugin` outlives this call.
        unsafe { (self.plugin.api.destroy)(self.handle.0) };
    }
}

fn create_decoder_handle(
    api: FrdFfmpegApiV1,
    config: &VideoStreamConfig,
) -> Result<SendDecoderHandle, VideoDecodeError> {
    let raw_config = abi_video_config(config);
    let mut handle = ptr::null_mut();
    // SAFETY: every pointer in `raw_config` borrows validated `config` storage for this call. The
    // ABI requires the plugin to copy any configuration bytes it retains.
    let status = unsafe { (api.create_decoder)(&raw_config, &mut handle) };
    if status != FrdStatus::OK || handle.is_null() {
        return Err(VideoDecodeError::new(
            VideoDecodeErrorCode::DecoderCreationFailed,
        ));
    }
    Ok(SendDecoderHandle(handle))
}

fn abi_video_config(config: &VideoStreamConfig) -> FrdVideoConfig {
    let input = config.as_input();
    FrdVideoConfig {
        codec: FRD_CODEC_HEVC,
        profile: FRD_PROFILE_HEVC_MAIN_444_8,
        chroma: FRD_CHROMA_YUV_444,
        bit_depth: u32::from(input.bit_depth),
        coded_width: input.coded_size.width,
        coded_height: input.coded_size.height,
        timebase: input.time_base.ticks_per_second().get(),
        bitstream_format: FRD_BITSTREAM_ANNEX_B,
        vps: byte_slice(input.parameter_sets.vps().unwrap_or_default()),
        sps: byte_slice(input.parameter_sets.sps()),
        pps: byte_slice(input.parameter_sets.pps()),
    }
}

fn byte_slice(bytes: &[u8]) -> FrdByteSlice {
    FrdByteSlice {
        data: bytes.as_ptr(),
        len: bytes.len(),
    }
}

fn convert_frame(
    config: &VideoStreamConfig,
    raw_frame: FrdDecodedFrame,
) -> Result<DecodedVideoFrame, VideoDecodeError> {
    let buffers = raw_frame.planes.map(|plane| PluginPlaneBuffer { plane });
    validate_raw_frame_layout(config, &raw_frame)?;
    let timestamp_ticks = u64::try_from(raw_frame.timestamp_ticks)
        .map_err(|_| VideoDecodeError::new(VideoDecodeErrorCode::DecodedFrameLayoutInvalid))?;
    let planes = buffers
        .iter()
        .map(PluginPlaneBuffer::copy_plane)
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice();
    let input = config.as_input();
    DecodedVideoFrame::try_new(DecodedVideoFrameInput {
        identity: input.identity,
        generation: input.generation,
        timestamp: VideoTimestamp {
            ticks: timestamp_ticks,
            timescale: input.time_base.ticks_per_second(),
        },
        coded_size: input.coded_size,
        visible_rect: input.visible_rect,
        format: VideoPixelFormat::Yuv444P8,
        planes,
    })
    .map_err(|_| VideoDecodeError::new(VideoDecodeErrorCode::DecodedFrameLayoutInvalid))
}

fn validate_raw_frame_layout(
    config: &VideoStreamConfig,
    raw_frame: &FrdDecodedFrame,
) -> Result<(), VideoDecodeError> {
    let invalid = || VideoDecodeError::new(VideoDecodeErrorCode::DecodedFrameLayoutInvalid);
    if raw_frame.pixel_format != FRD_PIXEL_FORMAT_YUV_444_P8 || raw_frame.plane_count != 3 {
        return Err(invalid());
    }

    let expected = config.as_input().coded_size;
    let mut total_bytes = 0usize;
    for plane in &raw_frame.planes {
        if plane.width != expected.width
            || plane.height != expected.height
            || plane.stride_bytes < plane.width
            || plane.buffer.data.is_null()
            || plane.buffer.len == 0
            || plane.buffer.release.is_none()
        {
            return Err(invalid());
        }
        let required = usize::try_from(plane.stride_bytes)
            .ok()
            .and_then(|stride| stride.checked_mul(plane.height as usize))
            .ok_or_else(invalid)?;
        if required > plane.buffer.len {
            return Err(invalid());
        }
        total_bytes = total_bytes
            .checked_add(plane.buffer.len)
            .ok_or_else(invalid)?;
        if total_bytes > MAX_DECODED_VIDEO_FRAME_BYTES {
            return Err(invalid());
        }
    }
    Ok(())
}

struct PluginPlaneBuffer {
    plane: crate::abi::FrdDecodedPlane,
}

impl PluginPlaneBuffer {
    fn copy_plane(&self) -> Result<VideoPlane, VideoDecodeError> {
        let FrdOwnedBuffer {
            data, len, release, ..
        } = self.plane.buffer;
        if data.is_null() || len == 0 || len > MAX_DECODED_VIDEO_FRAME_BYTES || release.is_none() {
            return Err(VideoDecodeError::new(
                VideoDecodeErrorCode::DecodedFrameLayoutInvalid,
            ));
        }
        // SAFETY: an `OK` frame guarantees that `data..data+len` is readable until its matching
        // release callback is invoked. Length is bounded before constructing the slice.
        let bytes = unsafe { slice::from_raw_parts(data, len) }
            .to_vec()
            .into_boxed_slice();
        VideoPlane::try_new(
            self.plane.width,
            self.plane.height,
            self.plane.stride_bytes,
            bytes,
        )
        .map_err(|_| VideoDecodeError::new(VideoDecodeErrorCode::DecodedFrameLayoutInvalid))
    }
}

impl Drop for PluginPlaneBuffer {
    fn drop(&mut self) {
        if let Some(release) = self.plane.buffer.release {
            // SAFETY: the callback and context originate with this exact buffer. This guard owns
            // the one required release and runs after any product-side copy attempt.
            unsafe {
                release(
                    self.plane.buffer.release_context,
                    self.plane.buffer.data,
                    self.plane.buffer.len,
                )
            };
        }
    }
}

fn canonical_codec_dir(application_dir: &Path) -> Result<PathBuf, VideoDecodeError> {
    if !application_dir.is_absolute()
        || application_dir
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(backend_unavailable());
    }

    let application_dir = application_dir
        .canonicalize()
        .map_err(|_| backend_unavailable())?;
    let requested_codec_dir = application_dir
        .join("codecs")
        .join("ffmpeg-8.1.2")
        .join(platform_directory_name());
    let codec_dir = requested_codec_dir
        .canonicalize()
        .map_err(|_| backend_unavailable())?;

    if !codec_dir.starts_with(&application_dir) || !codec_dir.is_dir() {
        return Err(backend_unavailable());
    }
    Ok(codec_dir)
}

fn canonical_library_path(codec_dir: &Path, name: &str) -> Result<PathBuf, VideoDecodeError> {
    let path = codec_dir
        .join(name)
        .canonicalize()
        .map_err(|_| backend_unavailable())?;
    if !path.is_file() || path.parent() != Some(codec_dir) {
        return Err(backend_unavailable());
    }
    Ok(path)
}

fn load_api(plugin: &Library) -> Result<FrdFfmpegApiV1, VideoDecodeError> {
    // SAFETY: `plugin` remains loaded while the symbol is read. The symbol name and function
    // signature are the versioned FreeRemoteDesk plugin ABI, and the returned table is copied
    // before the temporary `Symbol` borrow ends.
    let get_api: Symbol<FrdGetFfmpegApiV1> = unsafe {
        plugin
            .get(FRD_FFMPEG_API_SYMBOL)
            .map_err(|_| backend_unavailable())?
    };
    // SAFETY: a plugin from the canonical product codec directory owns this versioned export.
    // A null pointer means its native codec prerequisites are unavailable. The ABI version is
    // the first field and the table has static plugin lifetime by contract.
    let api = unsafe { get_api() };
    if api.is_null() {
        return Err(backend_unavailable());
    }
    // Read the leading version word before the full table so an older or newer layout is never
    // interpreted as `FrdFfmpegApiV1`.
    // SAFETY: the versioned export contract requires every non-null result to expose at least its
    // leading `u32` ABI version for negotiation.
    let abi_version = unsafe { api.cast::<u32>().read() };
    if abi_version != FRD_FFMPEG_ABI_VERSION {
        return Err(VideoDecodeError::new(
            VideoDecodeErrorCode::BackendVersionMismatch,
        ));
    }
    // SAFETY: after the leading version matches, the export contract guarantees an immutable
    // static `FrdFfmpegApiV1`. The library handle is retained for all copied function pointers.
    Ok(unsafe { api.read() })
}

#[cfg(windows)]
fn open_library(path: &Path) -> Result<Library, VideoDecodeError> {
    use libloading::os::windows::{
        Library as WindowsLibrary, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    };

    let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;
    // SAFETY: the path was canonicalized beneath the product codec directory. These flags make
    // LoadLibraryExW resolve adjacent FFmpeg dependencies and trusted default directories only.
    unsafe { WindowsLibrary::load_with_flags(path, flags) }
        .map(Into::into)
        .map_err(|_| backend_unavailable())
}

#[cfg(unix)]
fn open_library(path: &Path) -> Result<Library, VideoDecodeError> {
    use libloading::os::unix::{Library as UnixLibrary, RTLD_LOCAL, RTLD_NOW};

    // SAFETY: the path was canonicalized beneath the product codec directory. RTLD_NOW fails
    // before registration on unresolved symbols and RTLD_LOCAL prevents global symbol export.
    unsafe { UnixLibrary::open(Some(path), RTLD_NOW | RTLD_LOCAL) }
        .map(Into::into)
        .map_err(|_| backend_unavailable())
}

#[cfg(not(any(windows, unix)))]
fn open_library(_path: &Path) -> Result<Library, VideoDecodeError> {
    Err(backend_unavailable())
}

fn backend_unavailable() -> VideoDecodeError {
    VideoDecodeError::new(VideoDecodeErrorCode::BackendUnavailable)
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const fn platform_directory_name() -> &'static str {
    "windows-x86_64"
}

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
const fn platform_directory_name() -> &'static str {
    "windows-aarch64"
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const fn platform_directory_name() -> &'static str {
    "macos-x86_64"
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const fn platform_directory_name() -> &'static str {
    "macos-aarch64"
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const fn platform_directory_name() -> &'static str {
    "linux-x86_64"
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const fn platform_directory_name() -> &'static str {
    "linux-aarch64"
}

#[cfg(target_os = "android")]
const fn platform_directory_name() -> &'static str {
    "android"
}

#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    target_os = "android"
)))]
const fn platform_directory_name() -> &'static str {
    "unsupported"
}

#[cfg(windows)]
const fn ffmpeg_dependency_names() -> &'static [&'static str] {
    &["avutil-60.dll", "avcodec-62.dll"]
}

#[cfg(target_os = "macos")]
const fn ffmpeg_dependency_names() -> &'static [&'static str] {
    &["libavutil.60.dylib", "libavcodec.62.dylib"]
}

#[cfg(all(unix, not(target_os = "macos")))]
const fn ffmpeg_dependency_names() -> &'static [&'static str] {
    &["libavutil.so.60", "libavcodec.so.62"]
}

#[cfg(windows)]
const fn plugin_library_name() -> &'static str {
    "freeremotedesk_ffmpeg.dll"
}

#[cfg(target_os = "macos")]
const fn plugin_library_name() -> &'static str {
    "libfreeremotedesk_ffmpeg.dylib"
}

#[cfg(all(unix, not(target_os = "macos")))]
const fn plugin_library_name() -> &'static str {
    "libfreeremotedesk_ffmpeg.so"
}

#[cfg(not(any(windows, unix)))]
const fn ffmpeg_dependency_names() -> &'static [&'static str] {
    &[]
}

#[cfg(not(any(windows, unix)))]
const fn plugin_library_name() -> &'static str {
    "unsupported"
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use frd_core::{PixelRect, PixelSize, SessionId};
    use frd_media_api::{
        ChromaFormat, ChromaLocation, DecodeOutcome, EncodedVideoAccessUnit,
        VideoBackendAvailability, VideoBackendKind, VideoBitstreamFormat, VideoCapabilityProvider,
        VideoCodec, VideoColorimetry, VideoDecodeErrorCode, VideoDecodeQuery, VideoDecodeSupport,
        VideoDecoderFactory, VideoParameterSets, VideoPixelFormat, VideoProfile, VideoRange,
        VideoStreamConfig, VideoStreamConfigInput, VideoStreamIdentity, VideoTimeBase,
        VideoTimestamp,
    };

    use crate::abi::{
        FrdDecodedFrame, FrdDecodedPlane, FrdDecoderHandle, FrdFfmpegApiV1, FrdOwnedBuffer,
        FrdStatus, FrdVideoConfig, FRD_FFMPEG_AVCODEC_MAJOR, FRD_PIXEL_FORMAT_YUV_444_P8,
    };

    use super::{validate_raw_frame_layout, FfmpegBackend, FRD_FFMPEG_ABI_VERSION};

    #[test]
    fn missing_plugin_is_backend_unavailable_not_process_failure() {
        let result = FfmpegBackend::load_from(test_dir("missing"));

        assert_eq!(
            result.expect_err("缺失插件必须失败关闭").code(),
            VideoDecodeErrorCode::BackendUnavailable
        );
    }

    #[test]
    fn incompatible_plugin_abi_is_rejected_before_factory_registration() {
        let result = load_fake_plugin_with_abi(FRD_FFMPEG_ABI_VERSION + 1);

        assert_eq!(
            result.expect_err("不兼容 ABI 必须拒绝").code(),
            VideoDecodeErrorCode::BackendVersionMismatch
        );
    }

    #[test]
    fn incompatible_libavcodec_major_is_rejected_before_factory_registration() {
        let mut api = fake_api(FRD_FFMPEG_ABI_VERSION);
        api.avcodec_major = FRD_FFMPEG_AVCODEC_MAJOR + 1;

        let result = FfmpegBackend::from_api_for_test(api);

        assert_eq!(
            result.expect_err("不兼容 libavcodec 必须拒绝").code(),
            VideoDecodeErrorCode::BackendVersionMismatch
        );
    }

    #[test]
    fn relative_application_directory_is_rejected_without_searching_current_directory() {
        let result = FfmpegBackend::load_from(PathBuf::from("codecs"));

        assert_eq!(
            result.expect_err("相对目录必须拒绝").code(),
            VideoDecodeErrorCode::BackendUnavailable
        );
    }

    #[test]
    fn absolute_application_directory_with_parent_traversal_is_rejected() {
        let base = test_dir("parent-traversal");
        let traversing = base.join("child").join("..");

        let result = FfmpegBackend::load_from(traversing);

        assert_eq!(
            result.expect_err("包含父目录遍历的路径必须拒绝").code(),
            VideoDecodeErrorCode::BackendUnavailable
        );
    }

    #[test]
    fn compatible_plugin_is_decoder_ready_only_after_loading() {
        let backend = load_fake_plugin_with_abi(FRD_FFMPEG_ABI_VERSION)
            .expect("兼容且已加载的 API 应注册 factory");

        assert_eq!(backend.backend_kind(), VideoBackendKind::Ffmpeg);
        assert_eq!(
            backend.availability(),
            VideoBackendAvailability::DecoderReady
        );
    }

    #[test]
    fn compatible_plugin_advertises_main444_as_software_exact_only() {
        let backend = load_fake_plugin_with_abi(FRD_FFMPEG_ABI_VERSION)
            .expect("兼容且已加载的 API 应注册 factory");

        let support = backend.query(&main444_query());

        assert!(matches!(support, VideoDecodeSupport::SoftwareExact(_)));
        assert!(!matches!(support, VideoDecodeSupport::HardwareExact(_)));
    }

    #[test]
    fn plugin_decoder_creation_failure_returns_stable_error() {
        let backend = load_fake_plugin_with_abi(FRD_FFMPEG_ABI_VERSION)
            .expect("兼容且已加载的 API 应注册 factory");

        let error = match backend.create(&main444_config()) {
            Ok(_) => panic!("stub 不支持创建 decoder"),
            Err(error) => error,
        };

        assert_eq!(error.code(), VideoDecodeErrorCode::DecoderCreationFailed);
    }

    #[test]
    fn received_planes_are_released_by_the_originating_plugin_callbacks() {
        RELEASE_COUNT.store(0, Ordering::SeqCst);
        let backend =
            FfmpegBackend::from_api_for_test(ready_fake_api()).expect("兼容 fake plugin 应可加载");
        let config = main444_config();
        let input = config.as_input();
        let access_unit = EncodedVideoAccessUnit::try_new(
            input.identity,
            input.generation,
            VideoTimestamp {
                ticks: 1,
                timescale: input.time_base.ticks_per_second(),
            },
            true,
            vec![0, 0, 0, 1, 0x26].into_boxed_slice(),
        )
        .expect("测试访问单元有效");
        let mut decoder = backend.create(&config).expect("fake decoder 应可创建");

        let outcome = decoder.submit(access_unit).expect("fake frame 应可接收");

        assert!(matches!(outcome, DecodeOutcome::Frames(ref frames) if frames.len() == 1));
        assert_eq!(RELEASE_COUNT.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn aggregate_plugin_plane_budget_is_rejected_before_copying_buffers() {
        let oversized_half = frd_media_api::MAX_DECODED_VIDEO_FRAME_BYTES / 2 + 1;
        let plane = FrdDecodedPlane {
            width: 2,
            height: 2,
            stride_bytes: 2,
            buffer: FrdOwnedBuffer {
                data: std::ptr::NonNull::<u8>::dangling().as_ptr(),
                len: oversized_half,
                release: Some(stub_release),
                release_context: std::ptr::null_mut(),
            },
        };
        let frame = FrdDecodedFrame {
            timestamp_ticks: 1,
            pixel_format: FRD_PIXEL_FORMAT_YUV_444_P8,
            plane_count: 3,
            planes: [plane; 3],
        };

        let result = validate_raw_frame_layout(&main444_config(), &frame);

        assert_eq!(
            result.expect_err("三 plane 合计超预算必须拒绝").code(),
            VideoDecodeErrorCode::DecodedFrameLayoutInvalid
        );
    }

    fn load_fake_plugin_with_abi(
        abi_version: u32,
    ) -> Result<FfmpegBackend, frd_media_api::VideoDecodeError> {
        FfmpegBackend::from_api_for_test(fake_api(abi_version))
    }

    fn fake_api(abi_version: u32) -> FrdFfmpegApiV1 {
        FrdFfmpegApiV1 {
            abi_version,
            avcodec_major: FRD_FFMPEG_AVCODEC_MAJOR,
            create_decoder: stub_create,
            submit: stub_submit,
            receive: stub_receive,
            flush: stub_flush,
            destroy: stub_destroy,
        }
    }

    fn ready_fake_api() -> FrdFfmpegApiV1 {
        FrdFfmpegApiV1 {
            abi_version: FRD_FFMPEG_ABI_VERSION,
            avcodec_major: FRD_FFMPEG_AVCODEC_MAJOR,
            create_decoder: fake_create,
            submit: fake_submit,
            receive: fake_receive,
            flush: stub_flush,
            destroy: fake_destroy,
        }
    }

    struct FakeDecoderState {
        emitted: bool,
    }

    static RELEASE_COUNT: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn fake_create(
        _config: *const FrdVideoConfig,
        handle: *mut FrdDecoderHandle,
    ) -> FrdStatus {
        if handle.is_null() {
            return FrdStatus::INVALID_ARGUMENT;
        }
        let state = Box::new(FakeDecoderState { emitted: false });
        // SAFETY: the out pointer was checked and the allocation is transferred to `fake_destroy`.
        unsafe { handle.write(Box::into_raw(state).cast::<c_void>()) };
        FrdStatus::OK
    }

    unsafe extern "C" fn fake_submit(
        _handle: FrdDecoderHandle,
        _data: *const u8,
        _len: usize,
        _timestamp: i64,
        _flags: u32,
    ) -> FrdStatus {
        FrdStatus::OK
    }

    unsafe extern "C" fn fake_receive(
        handle: FrdDecoderHandle,
        frame: *mut FrdDecodedFrame,
    ) -> FrdStatus {
        if handle.is_null() || frame.is_null() {
            return FrdStatus::INVALID_ARGUMENT;
        }
        // SAFETY: `fake_create` allocated this exclusive state and product calls are serialized.
        let state = unsafe { &mut *handle.cast::<FakeDecoderState>() };
        if state.emitted {
            return FrdStatus::NEED_MORE_DATA;
        }
        state.emitted = true;
        let output = FrdDecodedFrame {
            timestamp_ticks: 1,
            pixel_format: FRD_PIXEL_FORMAT_YUV_444_P8,
            plane_count: 3,
            planes: [fake_plane(0x10), fake_plane(0x80), fake_plane(0x80)],
        };
        // SAFETY: the caller supplied writable storage and `OK` transfers all three buffers.
        unsafe { frame.write(output) };
        FrdStatus::OK
    }

    unsafe extern "C" fn fake_destroy(handle: FrdDecoderHandle) {
        if !handle.is_null() {
            // SAFETY: this allocation came from `fake_create` and is destroyed exactly once.
            drop(unsafe { Box::from_raw(handle.cast::<FakeDecoderState>()) });
        }
    }

    fn fake_plane(value: u8) -> FrdDecodedPlane {
        let allocation = Box::new(vec![value; 4]);
        let data = allocation.as_ptr();
        FrdDecodedPlane {
            width: 2,
            height: 2,
            stride_bytes: 2,
            buffer: FrdOwnedBuffer {
                data,
                len: allocation.len(),
                release: Some(fake_release),
                release_context: Box::into_raw(allocation).cast::<c_void>(),
            },
        }
    }

    unsafe extern "C" fn fake_release(context: *mut c_void, _data: *const u8, _len: usize) {
        // SAFETY: `fake_plane` transfers exactly one `Box<Vec<u8>>` into this callback context.
        drop(unsafe { Box::from_raw(context.cast::<Vec<u8>>()) });
        RELEASE_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "C" fn stub_create(
        _config: *const FrdVideoConfig,
        _handle: *mut FrdDecoderHandle,
    ) -> FrdStatus {
        FrdStatus::UNSUPPORTED
    }

    unsafe extern "C" fn stub_submit(
        _handle: FrdDecoderHandle,
        _data: *const u8,
        _len: usize,
        _timestamp: i64,
        _flags: u32,
    ) -> FrdStatus {
        FrdStatus::UNSUPPORTED
    }

    unsafe extern "C" fn stub_receive(
        _handle: FrdDecoderHandle,
        _frame: *mut FrdDecodedFrame,
    ) -> FrdStatus {
        FrdStatus::NEED_MORE_DATA
    }

    unsafe extern "C" fn stub_flush(_handle: FrdDecoderHandle) -> FrdStatus {
        FrdStatus::OK
    }

    unsafe extern "C" fn stub_destroy(_handle: FrdDecoderHandle) {}

    unsafe extern "C" fn stub_release(_context: *mut c_void, _data: *const u8, _len: usize) {}

    fn test_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir()
            .join("frd-video-ffmpeg-loader-tests")
            .join(name);
        std::fs::create_dir_all(&path).expect("应能创建测试应用目录");
        path.canonicalize().expect("测试应用目录应可 canonicalize")
    }

    fn main444_query() -> VideoDecodeQuery {
        VideoDecodeQuery {
            codec: VideoCodec::Hevc,
            profile: VideoProfile::HevcMain4448,
            chroma: ChromaFormat::Yuv444,
            bit_depth: 8,
            coded_size: PixelSize::new(1920, 1080).expect("测试尺寸有效"),
            frame_rate: None,
            preferred_outputs: vec![VideoPixelFormat::Yuv444P8].into_boxed_slice(),
        }
    }

    fn main444_config() -> VideoStreamConfig {
        VideoStreamConfig::try_new(VideoStreamConfigInput {
            identity: VideoStreamIdentity {
                session_id: SessionId::allocate(),
                stream_id: 7,
            },
            generation: 3,
            codec: VideoCodec::Hevc,
            profile: VideoProfile::HevcMain4448,
            chroma: ChromaFormat::Yuv444,
            bit_depth: 8,
            coded_size: PixelSize::new(2, 2).expect("测试尺寸有效"),
            visible_rect: PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
            time_base: VideoTimeBase::try_new(90_000).expect("测试 timebase 有效"),
            bitstream_format: VideoBitstreamFormat::AnnexB,
            colorimetry: VideoColorimetry::Bt709,
            range: VideoRange::Limited,
            chroma_location: ChromaLocation::Left,
            parameter_sets: VideoParameterSets::try_new(
                Some(vec![0x40].into_boxed_slice()),
                vec![0x42].into_boxed_slice(),
                vec![0x44].into_boxed_slice(),
            )
            .expect("测试参数集有效"),
        })
        .expect("测试配置有效")
    }
}
