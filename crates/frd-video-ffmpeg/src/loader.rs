use std::fmt;
use std::mem;
use std::path::Path;
#[cfg(test)]
use std::path::{Component, PathBuf};
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
    FrdByteSlice, FrdCreateDecoderFn, FrdDecodedFrame, FrdDecoderHandle, FrdDestroyFn, FrdFlushFn,
    FrdGetFfmpegApiV1, FrdOwnedBuffer, FrdReceiveFn, FrdReclaimFrameFn, FrdStatus, FrdSubmitFn,
    FrdVideoConfig, RawFrdFfmpegApiV1, FRD_API_CONTRACT_REQUIRED, FRD_BITSTREAM_ANNEX_B,
    FRD_CHROMA_YUV_444, FRD_CODEC_HEVC, FRD_FFMPEG_API_SYMBOL, FRD_FFMPEG_API_V1_ALIGNMENT,
    FRD_FFMPEG_API_V1_SIZE, FRD_FFMPEG_AVCODEC_MAJOR, FRD_PIXEL_FORMAT_YUV_444_P8,
    FRD_PROFILE_HEVC_MAIN_444_8, FRD_SUBMIT_RANDOM_ACCESS,
};

const MAX_DECODE_FRAMES_PER_BATCH: usize = 8;
const MAX_DECODE_BATCH_BYTES: usize = MAX_DECODED_VIDEO_FRAME_BYTES;

/// 已通过固定路径、ABI 和 libavcodec 版本门禁的可选 FFmpeg backend。
pub struct FfmpegBackend {
    plugin: Arc<LoadedPlugin>,
}

struct LoadedPlugin {
    api: CallableFfmpegApiV1,
    _libraries: Vec<Library>,
}

#[derive(Clone, Copy)]
struct CallableFfmpegApiV1 {
    abi_version: u32,
    avcodec_major: u32,
    create_decoder: FrdCreateDecoderFn,
    submit: FrdSubmitFn,
    receive: FrdReceiveFn,
    flush: FrdFlushFn,
    destroy: FrdDestroyFn,
    reclaim_frame: FrdReclaimFrameFn,
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
    /// 从当前可执行文件目录下的固定、受信版本目录加载；失败只禁用该 backend。
    pub fn load() -> Result<Self, VideoDecodeError> {
        #[cfg(not(windows))]
        {
            return Err(backend_unavailable());
        }

        #[cfg(windows)]
        {
            let executable = std::env::current_exe().map_err(|_| backend_unavailable())?;
            let application_dir = executable.parent().ok_or_else(backend_unavailable)?;
            let trusted = crate::trusted_path::prepare(
                application_dir,
                platform_directory_name(),
                ffmpeg_dependency_names(),
                plugin_library_name(),
            )
            .map_err(|_| backend_unavailable())?;
            let mut libraries = Vec::new();
            for path in &trusted.dependencies {
                libraries.push(open_library(path)?);
            }
            let plugin = open_library(&trusted.plugin)?;
            let api = load_api(&plugin)?;
            libraries.push(plugin);
            Self::from_validated_api(api, libraries)
        }
    }

    /// 测试专用路径注入，不执行生产受信 ACL/owner 门禁。
    #[cfg(test)]
    fn load_from_application_dir_for_test(
        application_dir: impl AsRef<Path>,
    ) -> Result<Self, VideoDecodeError> {
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

        Self::from_validated_api(api, libraries)
    }

    fn from_validated_api(
        api: CallableFfmpegApiV1,
        libraries: Vec<Library>,
    ) -> Result<Self, VideoDecodeError> {
        Ok(Self {
            plugin: Arc::new(LoadedPlugin {
                api,
                _libraries: libraries,
            }),
        })
    }

    #[cfg(test)]
    fn from_raw_api_for_test(api: RawFrdFfmpegApiV1) -> Result<Self, VideoDecodeError> {
        let api = validate_raw_api(api)?;
        Self::from_validated_api(api, Vec::new())
    }
}

#[cfg(all(test, windows))]
fn verify_trusted_install_chain(application_dir: &Path) -> Result<(), VideoDecodeError> {
    crate::trusted_path::verify_install_root(application_dir).map_err(|_| backend_unavailable())
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
    pending_frame: Option<DecodedVideoFrame>,
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
            pending_frame: None,
        })
    }

    fn receive_frames(&mut self) -> Result<Box<[DecodedVideoFrame]>, VideoDecodeError> {
        self.receive_frames_with_limits(MAX_DECODE_FRAMES_PER_BATCH, MAX_DECODE_BATCH_BYTES)
    }

    fn receive_frames_with_limits(
        &mut self,
        max_frames: usize,
        max_bytes: usize,
    ) -> Result<Box<[DecodedVideoFrame]>, VideoDecodeError> {
        if let Some(frame) = self.pending_frame.take() {
            return Ok(vec![frame].into_boxed_slice());
        }

        let mut frames = Vec::new();
        let mut batch_bytes = 0usize;

        while frames.len() < max_frames {
            let mut raw_frame = FrdDecodedFrame::default();
            // SAFETY: output storage is host-owned and zeroed. The validated ABI requires
            // `reclaim_frame` to accept this storage after every status, including partial error.
            let status = unsafe { (self.plugin.api.receive)(self.handle.0, &mut raw_frame) };
            let raw_frame = ReceivedFrameGuard {
                api: self.plugin.api,
                handle: self.handle.0,
                frame: raw_frame,
            };
            if status == FrdStatus::NEED_MORE_DATA || status == FrdStatus::END_OF_STREAM {
                return Ok(frames.into_boxed_slice());
            }
            if status != FrdStatus::OK {
                return Err(self.decode_error());
            }
            let frame = convert_frame(&self.config, &raw_frame.frame)?;
            let frame_bytes = decoded_frame_bytes(&frame);
            if !frames.is_empty()
                && !frame_fits_decode_batch_with_limit(batch_bytes, frame_bytes, max_bytes)
            {
                self.pending_frame = Some(frame);
                return Ok(frames.into_boxed_slice());
            }
            batch_bytes = batch_bytes
                .checked_add(frame_bytes)
                .ok_or_else(|| self.decode_error())?;
            self.emitted_frame = true;
            frames.push(frame);
        }

        Ok(frames.into_boxed_slice())
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
        self.pending_frame = None;
        Ok(())
    }
}

fn decoded_frame_bytes(frame: &DecodedVideoFrame) -> usize {
    frame
        .as_input()
        .planes
        .iter()
        .map(|plane| plane.bytes().len())
        .sum()
}

#[cfg(test)]
fn frame_fits_decode_batch(current_bytes: usize, next_frame_bytes: usize) -> bool {
    frame_fits_decode_batch_with_limit(current_bytes, next_frame_bytes, MAX_DECODE_BATCH_BYTES)
}

fn frame_fits_decode_batch_with_limit(
    current_bytes: usize,
    next_frame_bytes: usize,
    max_bytes: usize,
) -> bool {
    current_bytes
        .checked_add(next_frame_bytes)
        .is_some_and(|total| total <= max_bytes)
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        // SAFETY: this is the only owner of the non-null handle and `plugin` outlives this call.
        unsafe { (self.plugin.api.destroy)(self.handle.0) };
    }
}

fn create_decoder_handle(
    api: CallableFfmpegApiV1,
    config: &VideoStreamConfig,
) -> Result<SendDecoderHandle, VideoDecodeError> {
    let raw_config = abi_video_config(config);
    let mut handle = ptr::null_mut();
    // SAFETY: every pointer in `raw_config` borrows validated `config` storage for this call. The
    // ABI requires the plugin to copy any configuration bytes it retains.
    let status = unsafe { (api.create_decoder)(&raw_config, &mut handle) };
    if status != FrdStatus::OK {
        if !handle.is_null() {
            // SAFETY: the validated CREATE_NON_NULL_OWNED contract makes every non-null output
            // host-owned and destroyable even when create returns an error.
            unsafe { (api.destroy)(handle) };
        }
        return Err(VideoDecodeError::new(
            VideoDecodeErrorCode::DecoderCreationFailed,
        ));
    }
    if handle.is_null() {
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
    raw_frame: &FrdDecodedFrame,
) -> Result<DecodedVideoFrame, VideoDecodeError> {
    validate_raw_frame_layout(config, raw_frame)?;
    let timestamp_ticks = u64::try_from(raw_frame.timestamp_ticks)
        .map_err(|_| VideoDecodeError::new(VideoDecodeErrorCode::DecodedFrameLayoutInvalid))?;
    let planes = raw_frame
        .planes
        .iter()
        .map(copy_plugin_plane)
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
        colorimetry: input.colorimetry,
        range: input.range,
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

struct ReceivedFrameGuard {
    api: CallableFfmpegApiV1,
    handle: FrdDecoderHandle,
    frame: FrdDecodedFrame,
}

impl Drop for ReceivedFrameGuard {
    fn drop(&mut self) {
        // SAFETY: the validated contract requires this callback to reclaim host-zeroed, partial,
        // successful and error outputs. This guard is created once per receive and drops once.
        unsafe { (self.api.reclaim_frame)(self.handle, &mut self.frame) };
    }
}

fn copy_plugin_plane(plane: &crate::abi::FrdDecodedPlane) -> Result<VideoPlane, VideoDecodeError> {
    let FrdOwnedBuffer { data, len } = plane.buffer;
    if data.is_null() || len == 0 || len > MAX_DECODED_VIDEO_FRAME_BYTES {
        return Err(VideoDecodeError::new(
            VideoDecodeErrorCode::DecodedFrameLayoutInvalid,
        ));
    }
    // SAFETY: validated layout guarantees `data..data+len` is readable until the enclosing
    // receive guard invokes the plugin's frame-level reclaim callback.
    let bytes = unsafe { slice::from_raw_parts(data, len) }
        .to_vec()
        .into_boxed_slice();
    VideoPlane::try_new(plane.width, plane.height, plane.stride_bytes, bytes)
        .map_err(|_| VideoDecodeError::new(VideoDecodeErrorCode::DecodedFrameLayoutInvalid))
}

#[cfg(test)]
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

#[cfg(test)]
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

fn load_api(plugin: &Library) -> Result<CallableFfmpegApiV1, VideoDecodeError> {
    // SAFETY: `plugin` remains loaded while the symbol is read. The symbol name and function
    // signature are the versioned FreeRemoteDesk plugin ABI, and the returned table is copied
    // before the temporary `Symbol` borrow ends.
    let get_api: Symbol<FrdGetFfmpegApiV1> = unsafe {
        plugin
            .get(FRD_FFMPEG_API_SYMBOL)
            .map_err(|_| backend_unavailable())?
    };
    let mut raw_api = RawFrdFfmpegApiV1::default();
    // SAFETY: the host owns correctly aligned, zeroed storage of exactly the advertised size.
    // The getter contract forbids writes beyond `output_size`; all output fields are integers, so
    // partial or arbitrary C writes remain valid Rust data until explicit validation below.
    let status = unsafe { get_api(&mut raw_api, mem::size_of::<RawFrdFfmpegApiV1>()) };
    if status != FrdStatus::OK {
        return Err(backend_unavailable());
    }
    validate_raw_api(raw_api)
}

fn validate_raw_api(raw: RawFrdFfmpegApiV1) -> Result<CallableFfmpegApiV1, VideoDecodeError> {
    if raw.struct_size != FRD_FFMPEG_API_V1_SIZE
        || raw.struct_alignment != FRD_FFMPEG_API_V1_ALIGNMENT
        || raw.abi_version != FRD_FFMPEG_ABI_VERSION
        || raw.avcodec_major != FRD_FFMPEG_AVCODEC_MAJOR
        || raw.contract_flags & FRD_API_CONTRACT_REQUIRED != FRD_API_CONTRACT_REQUIRED
        || raw.reserved != 0
        || raw.create_decoder == 0
        || raw.submit == 0
        || raw.receive == 0
        || raw.flush == 0
        || raw.destroy == 0
        || raw.reclaim_frame == 0
    {
        return Err(VideoDecodeError::new(
            VideoDecodeErrorCode::BackendVersionMismatch,
        ));
    }

    const _: () = assert!(mem::size_of::<usize>() == mem::size_of::<FrdCreateDecoderFn>());
    const _: () = assert!(mem::size_of::<usize>() == mem::size_of::<FrdSubmitFn>());
    const _: () = assert!(mem::size_of::<usize>() == mem::size_of::<FrdReceiveFn>());
    const _: () = assert!(mem::size_of::<usize>() == mem::size_of::<FrdFlushFn>());
    const _: () = assert!(mem::size_of::<usize>() == mem::size_of::<FrdDestroyFn>());
    const _: () = assert!(mem::size_of::<usize>() == mem::size_of::<FrdReclaimFrameFn>());

    // SAFETY: every slot is a non-zero `uintptr_t` written by the trusted, version-matched plugin
    // getter. Size/alignment/contract validation completed before any integer is converted into a
    // callable Rust value. The plugin image is retained for the lifetime of these pointers.
    Ok(unsafe {
        CallableFfmpegApiV1 {
            abi_version: raw.abi_version,
            avcodec_major: raw.avcodec_major,
            create_decoder: mem::transmute::<usize, FrdCreateDecoderFn>(raw.create_decoder),
            submit: mem::transmute::<usize, FrdSubmitFn>(raw.submit),
            receive: mem::transmute::<usize, FrdReceiveFn>(raw.receive),
            flush: mem::transmute::<usize, FrdFlushFn>(raw.flush),
            destroy: mem::transmute::<usize, FrdDestroyFn>(raw.destroy),
            reclaim_frame: mem::transmute::<usize, FrdReclaimFrameFn>(raw.reclaim_frame),
        }
    })
}

#[cfg(windows)]
fn open_library(path: &Path) -> Result<Library, VideoDecodeError> {
    use libloading::os::windows::{
        Library as WindowsLibrary, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    };

    let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;
    // SAFETY: the production path is canonical, handle-identity checked, non-reparse, and held
    // open beneath a trusted install chain. These flags resolve adjacent FFmpeg dependencies and
    // trusted default directories only.
    unsafe { WindowsLibrary::load_with_flags(path, flags) }
        .map(Into::into)
        .map_err(|_| backend_unavailable())
}

#[cfg(unix)]
fn open_library(path: &Path) -> Result<Library, VideoDecodeError> {
    use libloading::os::unix::{Library as UnixLibrary, RTLD_LOCAL, RTLD_NOW};

    // SAFETY: the production path is canonical, identity checked, non-symlink, and rooted under
    // non-writable system-owned ancestors. RTLD_NOW fails on unresolved symbols and RTLD_LOCAL
    // prevents global symbol export.
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
    use std::sync::{Arc, Mutex};

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
        FrdDecodedFrame, FrdDecodedPlane, FrdDecoderHandle, FrdOwnedBuffer, FrdStatus,
        FrdVideoConfig, RawFrdFfmpegApiV1, FRD_API_CONTRACT_REQUIRED, FRD_FFMPEG_API_V1_ALIGNMENT,
        FRD_FFMPEG_API_V1_SIZE, FRD_FFMPEG_AVCODEC_MAJOR, FRD_PIXEL_FORMAT_YUV_444_P8,
    };

    #[cfg(windows)]
    use super::verify_trusted_install_chain;
    #[cfg(windows)]
    use super::{
        ffmpeg_dependency_names, load_api, open_library, platform_directory_name,
        plugin_library_name,
    };
    use super::{
        frame_fits_decode_batch, validate_raw_frame_layout, FfmpegBackend, FfmpegDecoder,
        FRD_FFMPEG_ABI_VERSION, MAX_DECODE_BATCH_BYTES,
    };

    #[test]
    fn missing_plugin_is_backend_unavailable_not_process_failure() {
        let result = FfmpegBackend::load_from_application_dir_for_test(test_dir("missing"));

        assert_eq!(
            result.expect_err("缺失插件必须失败关闭").code(),
            VideoDecodeErrorCode::BackendUnavailable
        );
    }

    #[test]
    fn production_loader_is_bound_to_the_current_executable_directory() {
        let result = FfmpegBackend::load();

        assert!(matches!(
            result,
            Err(ref error) if error.code() == VideoDecodeErrorCode::BackendUnavailable
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_public_loader_fails_closed_until_a_platform_trust_adapter_exists() {
        assert_eq!(
            FfmpegBackend::load()
                .expect_err("非 Windows 尚无受信安装 adapter")
                .code(),
            VideoDecodeErrorCode::BackendUnavailable
        );
    }

    #[cfg(windows)]
    #[test]
    fn writable_test_directory_is_not_a_trusted_install_chain() {
        let root = test_dir("untrusted-install-root");
        std::fs::create_dir_all(&root).expect("测试目录应创建");

        let result = verify_trusted_install_chain(&root);

        assert_eq!(
            result.expect_err("当前用户可写目录必须拒绝").code(),
            VideoDecodeErrorCode::BackendUnavailable
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_system_directory_satisfies_the_trusted_owner_and_acl_policy() {
        let system_root = std::env::var_os("SystemRoot").expect("Windows 必须提供 SystemRoot");
        let system_directory = PathBuf::from(system_root).join("System32");

        verify_trusted_install_chain(&system_directory)
            .expect("System32 应满足受信 owner/DACL 门禁");
    }

    #[cfg(windows)]
    #[test]
    fn windows_production_trust_and_loader_path_accepts_three_system_libraries() {
        let production_root = PathBuf::from(r"C:\Program Files\FreeRemoteDesk");
        assert_eq!(
            crate::trusted_path::production_codec_dir_for_test(
                &production_root,
                platform_directory_name(),
            ),
            production_root
                .join("codecs")
                .join("ffmpeg-8.1.2")
                .join("windows-x86_64")
        );
        assert_eq!(
            ffmpeg_dependency_names(),
            &["avutil-60.dll", "avcodec-62.dll"]
        );
        assert_eq!(plugin_library_name(), "freeremotedesk_ffmpeg.dll");

        let system_root =
            PathBuf::from(std::env::var_os("SystemRoot").expect("Windows 必须提供 SystemRoot"));
        let system_directory = system_root.join("System32");
        let trusted = crate::trusted_path::prepare_existing_platform_for_test(
            &system_root,
            &system_directory,
            &["kernel32.dll", "user32.dll"],
            "advapi32.dll",
        )
        .expect("完整 SystemRoot/System32 链和三个 DLL 应通过生产信任核心");
        assert_eq!(trusted.dependencies.len(), 2);
        assert_eq!(trusted.retained_object_count_for_test(), 6);

        let dependencies = trusted
            .dependencies
            .iter()
            .map(|path| open_library(path).expect("LoadLibraryEx 应安全加载受信依赖"))
            .collect::<Vec<_>>();
        assert_eq!(dependencies.len(), 2);
        let plugin = open_library(&trusted.plugin).expect("LoadLibraryEx 应安全加载受信第三个 DLL");

        let error = match load_api(&plugin) {
            Ok(_) => panic!("system DLL 不应暴露 FreeRemoteDesk plugin 符号"),
            Err(error) => error,
        };
        assert_eq!(error.code(), VideoDecodeErrorCode::BackendUnavailable);
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

        let result = FfmpegBackend::from_raw_api_for_test(api);

        assert_eq!(
            result.expect_err("不兼容 libavcodec 必须拒绝").code(),
            VideoDecodeErrorCode::BackendVersionMismatch
        );
    }

    #[test]
    fn raw_api_rejects_every_null_required_callback_before_typed_conversion() {
        for callback_index in 0..6 {
            let mut api = compatible_raw_api();
            match callback_index {
                0 => api.create_decoder = 0,
                1 => api.submit = 0,
                2 => api.receive = 0,
                3 => api.flush = 0,
                4 => api.destroy = 0,
                5 => api.reclaim_frame = 0,
                _ => unreachable!(),
            }

            let result = FfmpegBackend::from_raw_api_for_test(api);

            assert_eq!(
                result.expect_err("required callback 为空必须拒绝").code(),
                VideoDecodeErrorCode::BackendVersionMismatch,
                "callback index {callback_index}"
            );
        }
    }

    #[test]
    fn raw_api_rejects_size_alignment_and_threading_contract_mismatch() {
        let mut wrong_size = compatible_raw_api();
        wrong_size.struct_size -= 1;
        let mut wrong_alignment = compatible_raw_api();
        wrong_alignment.struct_alignment *= 2;
        let mut missing_contract = compatible_raw_api();
        missing_contract.contract_flags = 0;

        for api in [wrong_size, wrong_alignment, missing_contract] {
            let result = FfmpegBackend::from_raw_api_for_test(api);
            assert_eq!(
                result
                    .expect_err("ABI header/contract 不匹配必须拒绝")
                    .code(),
                VideoDecodeErrorCode::BackendVersionMismatch
            );
        }
    }

    #[test]
    fn relative_application_directory_is_rejected_without_searching_current_directory() {
        let result = FfmpegBackend::load_from_application_dir_for_test(PathBuf::from("codecs"));

        assert_eq!(
            result.expect_err("相对目录必须拒绝").code(),
            VideoDecodeErrorCode::BackendUnavailable
        );
    }

    #[test]
    fn absolute_application_directory_with_parent_traversal_is_rejected() {
        let base = test_dir("parent-traversal");
        let traversing = base.join("child").join("..");

        let result = FfmpegBackend::load_from_application_dir_for_test(traversing);

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
    fn non_ok_create_with_non_null_handle_is_destroyed_exactly_once() {
        CREATE_FAILURE_DESTROY_COUNT.store(0, Ordering::SeqCst);
        let mut api = compatible_raw_api();
        api.create_decoder = failing_create_with_handle as *const () as usize;
        api.destroy = destroy_failed_create_handle as *const () as usize;
        let backend = FfmpegBackend::from_raw_api_for_test(api).expect("兼容 fake API 应加载");

        let result = backend.create(&main444_config());

        assert!(matches!(
            result,
            Err(ref error) if error.code() == VideoDecodeErrorCode::DecoderCreationFailed
        ));
        assert_eq!(CREATE_FAILURE_DESTROY_COUNT.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn received_planes_are_released_by_the_originating_plugin_callbacks() {
        let _guard = FAKE_FRAME_TEST_LOCK.lock().expect("fake frame 测试锁有效");
        RELEASE_COUNT.store(0, Ordering::SeqCst);
        FRAME_RECLAIM_CALL_COUNT.store(0, Ordering::SeqCst);
        let backend = FfmpegBackend::from_raw_api_for_test(ready_fake_api())
            .expect("兼容 fake plugin 应可加载");
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
        let DecodeOutcome::Frames(frames) = outcome else {
            unreachable!("上方断言已锁定单帧输出")
        };
        assert_eq!(frames[0].as_input().colorimetry, VideoColorimetry::Bt709);
        assert_eq!(frames[0].as_input().range, VideoRange::Limited);
        assert_eq!(RELEASE_COUNT.load(Ordering::SeqCst), 3);
        assert_eq!(FRAME_RECLAIM_CALL_COUNT.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn validation_failure_reclaims_partial_frame_exactly_once() {
        assert_partial_frame_reclaimed_once(
            receive_malformed_partial_frame,
            VideoDecodeErrorCode::DecodedFrameLayoutInvalid,
        );
    }

    #[test]
    fn plugin_receive_error_reclaims_partial_frame_exactly_once() {
        assert_partial_frame_reclaimed_once(
            receive_error_with_partial_frame,
            VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame,
        );
    }

    #[test]
    fn frame_count_cap_returns_valid_batch_and_leaves_output_for_next_drain() {
        let _guard = FAKE_FRAME_TEST_LOCK.lock().expect("fake frame 测试锁有效");
        let backend =
            FfmpegBackend::from_raw_api_for_test(burst_fake_api()).expect("兼容 burst API 应加载");
        let config = main444_config();
        let mut decoder = backend.create(&config).expect("burst decoder 应创建");

        let first = decoder
            .submit(test_access_unit(&config))
            .expect("达到 cap 应返回有效批次");
        let second = decoder.flush().expect("下一次 drain 应取回余下帧");

        assert!(matches!(first, DecodeOutcome::Frames(ref frames) if frames.len() == 8));
        assert_eq!(second.len(), 2);
    }

    #[test]
    fn aggregate_batch_byte_budget_defers_overflowing_frame() {
        assert!(frame_fits_decode_batch(MAX_DECODE_BATCH_BYTES - 5, 5));
        assert!(!frame_fits_decode_batch(MAX_DECODE_BATCH_BYTES - 5, 6));
        assert!(!frame_fits_decode_batch(usize::MAX, 1));

        let _guard = FAKE_FRAME_TEST_LOCK.lock().expect("fake frame 测试锁有效");
        let backend =
            FfmpegBackend::from_raw_api_for_test(burst_fake_api()).expect("兼容 burst API 应加载");
        let mut decoder = FfmpegDecoder::create(Arc::clone(&backend.plugin), main444_config())
            .expect("burst decoder 应创建");

        let first = decoder
            .receive_frames_with_limits(8, 12)
            .expect("首个 12-byte frame 应返回");
        let second = decoder
            .receive_frames_with_limits(8, 12)
            .expect("超预算 frame 应留待下一次 drain");

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn deferred_frame_is_returned_before_a_later_plugin_receive_error() {
        let _guard = FAKE_FRAME_TEST_LOCK.lock().expect("fake frame 测试锁有效");
        RELEASE_COUNT.store(0, Ordering::SeqCst);
        FRAME_RECLAIM_CALL_COUNT.store(0, Ordering::SeqCst);
        let backend = FfmpegBackend::from_raw_api_for_test(deferred_then_error_fake_api())
            .expect("兼容 deferred-error API 应加载");
        let mut decoder = FfmpegDecoder::create(Arc::clone(&backend.plugin), main444_config())
            .expect("deferred-error decoder 应创建");

        let first = decoder
            .receive_frames_with_limits(8, 12)
            .expect("首帧应返回并延后第二帧");
        let deferred = decoder
            .receive_frames_with_limits(8, 12)
            .expect("延后帧必须在后续 plugin 错误前独立返回");
        let error = decoder
            .receive_frames_with_limits(8, 12)
            .expect_err("再下一次 drain 才应暴露 plugin 错误");

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].as_input().timestamp.ticks, 1);
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].as_input().timestamp.ticks, 2);
        assert_eq!(
            error.code(),
            VideoDecodeErrorCode::DecodeFailedAfterFirstFrame
        );
        assert_eq!(FRAME_RECLAIM_CALL_COUNT.load(Ordering::SeqCst), 3);
        assert_eq!(RELEASE_COUNT.load(Ordering::SeqCst), 6);
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
        FfmpegBackend::from_raw_api_for_test(fake_api(abi_version))
    }

    fn fake_api(abi_version: u32) -> RawFrdFfmpegApiV1 {
        let mut api = compatible_raw_api();
        api.abi_version = abi_version;
        api
    }

    fn compatible_raw_api() -> RawFrdFfmpegApiV1 {
        RawFrdFfmpegApiV1 {
            struct_size: FRD_FFMPEG_API_V1_SIZE,
            struct_alignment: FRD_FFMPEG_API_V1_ALIGNMENT,
            abi_version: FRD_FFMPEG_ABI_VERSION,
            avcodec_major: FRD_FFMPEG_AVCODEC_MAJOR,
            contract_flags: FRD_API_CONTRACT_REQUIRED,
            reserved: 0,
            create_decoder: stub_create as *const () as usize,
            submit: stub_submit as *const () as usize,
            receive: stub_receive as *const () as usize,
            flush: stub_flush as *const () as usize,
            destroy: stub_destroy as *const () as usize,
            reclaim_frame: stub_reclaim_frame as *const () as usize,
        }
    }

    fn ready_fake_api() -> RawFrdFfmpegApiV1 {
        let mut api = compatible_raw_api();
        api.create_decoder = fake_create as *const () as usize;
        api.submit = fake_submit as *const () as usize;
        api.receive = fake_receive as *const () as usize;
        api.destroy = fake_destroy as *const () as usize;
        api.reclaim_frame = fake_reclaim_frame as *const () as usize;
        api
    }

    fn burst_fake_api() -> RawFrdFfmpegApiV1 {
        let mut api = ready_fake_api();
        api.create_decoder = burst_create as *const () as usize;
        api.submit = burst_submit as *const () as usize;
        api.receive = burst_receive as *const () as usize;
        api.destroy = burst_destroy as *const () as usize;
        api
    }

    fn deferred_then_error_fake_api() -> RawFrdFfmpegApiV1 {
        let mut api = ready_fake_api();
        api.create_decoder = deferred_then_error_create as *const () as usize;
        api.receive = deferred_then_error_receive as *const () as usize;
        api.destroy = deferred_then_error_destroy as *const () as usize;
        api
    }

    struct FakeDecoderState {
        emitted: bool,
    }

    struct BurstDecoderState {
        remaining: usize,
    }

    struct DeferredThenErrorState {
        receive_calls: usize,
    }

    static RELEASE_COUNT: AtomicUsize = AtomicUsize::new(0);
    static FRAME_RECLAIM_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
    static FAKE_FRAME_TEST_LOCK: Mutex<()> = Mutex::new(());
    static CREATE_FAILURE_DESTROY_COUNT: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn failing_create_with_handle(
        _config: *const FrdVideoConfig,
        handle: *mut FrdDecoderHandle,
    ) -> FrdStatus {
        if handle.is_null() {
            return FrdStatus::INVALID_ARGUMENT;
        }
        let allocation = Box::new(7_u8);
        // SAFETY: checked out pointer; ownership transfers to required destroy-on-non-null rule.
        unsafe { handle.write(Box::into_raw(allocation).cast::<c_void>()) };
        FrdStatus::DECODE_FAILED
    }

    unsafe extern "C" fn destroy_failed_create_handle(handle: FrdDecoderHandle) {
        if !handle.is_null() {
            // SAFETY: handle came from `failing_create_with_handle` and must be reclaimed once.
            drop(unsafe { Box::from_raw(handle.cast::<u8>()) });
            CREATE_FAILURE_DESTROY_COUNT.fetch_add(1, Ordering::SeqCst);
        }
    }

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

    unsafe extern "C" fn burst_create(
        _config: *const FrdVideoConfig,
        handle: *mut FrdDecoderHandle,
    ) -> FrdStatus {
        if handle.is_null() {
            return FrdStatus::INVALID_ARGUMENT;
        }
        let state = Box::new(BurstDecoderState { remaining: 10 });
        // SAFETY: checked out pointer; allocation ownership transfers to `burst_destroy`.
        unsafe { handle.write(Box::into_raw(state).cast::<c_void>()) };
        FrdStatus::OK
    }

    unsafe extern "C" fn deferred_then_error_create(
        _config: *const FrdVideoConfig,
        handle: *mut FrdDecoderHandle,
    ) -> FrdStatus {
        if handle.is_null() {
            return FrdStatus::INVALID_ARGUMENT;
        }
        let state = Box::new(DeferredThenErrorState { receive_calls: 0 });
        // SAFETY: checked out pointer; allocation ownership transfers to the matching destroy.
        unsafe { handle.write(Box::into_raw(state).cast::<c_void>()) };
        FrdStatus::OK
    }

    unsafe extern "C" fn burst_submit(
        _handle: FrdDecoderHandle,
        _data: *const u8,
        _len: usize,
        _timestamp: i64,
        _flags: u32,
    ) -> FrdStatus {
        FrdStatus::OK
    }

    unsafe extern "C" fn burst_receive(
        handle: FrdDecoderHandle,
        frame: *mut FrdDecodedFrame,
    ) -> FrdStatus {
        if handle.is_null() || frame.is_null() {
            return FrdStatus::INVALID_ARGUMENT;
        }
        // SAFETY: exclusive state came from `burst_create`.
        let state = unsafe { &mut *handle.cast::<BurstDecoderState>() };
        if state.remaining == 0 {
            return FrdStatus::NEED_MORE_DATA;
        }
        state.remaining -= 1;
        let output = FrdDecodedFrame {
            timestamp_ticks: (10 - state.remaining) as i64,
            pixel_format: FRD_PIXEL_FORMAT_YUV_444_P8,
            plane_count: 3,
            planes: [fake_plane(0x10), fake_plane(0x80), fake_plane(0x80)],
        };
        // SAFETY: caller supplied host-owned writable output.
        unsafe { frame.write(output) };
        FrdStatus::OK
    }

    unsafe extern "C" fn deferred_then_error_receive(
        handle: FrdDecoderHandle,
        frame: *mut FrdDecodedFrame,
    ) -> FrdStatus {
        if handle.is_null() || frame.is_null() {
            return FrdStatus::INVALID_ARGUMENT;
        }
        // SAFETY: exclusive state came from `deferred_then_error_create`.
        let state = unsafe { &mut *handle.cast::<DeferredThenErrorState>() };
        state.receive_calls += 1;
        if state.receive_calls > 2 {
            return FrdStatus::DECODE_FAILED;
        }
        let output = FrdDecodedFrame {
            timestamp_ticks: state.receive_calls as i64,
            pixel_format: FRD_PIXEL_FORMAT_YUV_444_P8,
            plane_count: 3,
            planes: [fake_plane(0x10), fake_plane(0x80), fake_plane(0x80)],
        };
        // SAFETY: caller supplied host-owned writable output.
        unsafe { frame.write(output) };
        FrdStatus::OK
    }

    unsafe extern "C" fn burst_destroy(handle: FrdDecoderHandle) {
        if !handle.is_null() {
            // SAFETY: allocation came from `burst_create` and is destroyed once.
            drop(unsafe { Box::from_raw(handle.cast::<BurstDecoderState>()) });
        }
    }

    unsafe extern "C" fn deferred_then_error_destroy(handle: FrdDecoderHandle) {
        if !handle.is_null() {
            // SAFETY: allocation came from `deferred_then_error_create` and is destroyed once.
            drop(unsafe { Box::from_raw(handle.cast::<DeferredThenErrorState>()) });
        }
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
        let allocation = vec![value; 4].into_boxed_slice();
        let len = allocation.len();
        let data = Box::into_raw(allocation).cast::<u8>();
        FrdDecodedPlane {
            width: 2,
            height: 2,
            stride_bytes: 2,
            buffer: FrdOwnedBuffer { data, len },
        }
    }

    unsafe extern "C" fn fake_reclaim_frame(
        _handle: FrdDecoderHandle,
        frame: *mut FrdDecodedFrame,
    ) {
        if frame.is_null() {
            return;
        }
        FRAME_RECLAIM_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
        // SAFETY: host supplies writable frame storage. Every non-null plane was allocated by
        // `fake_plane`; clearing it makes this callback idempotent for partial/zero output.
        for plane in unsafe { &mut (*frame).planes } {
            if !plane.buffer.data.is_null() && plane.buffer.len != 0 {
                let allocation = std::ptr::slice_from_raw_parts_mut(
                    plane.buffer.data.cast_mut(),
                    plane.buffer.len,
                );
                drop(unsafe { Box::from_raw(allocation) });
                plane.buffer = FrdOwnedBuffer::default();
                RELEASE_COUNT.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    fn assert_partial_frame_reclaimed_once(
        receive: unsafe extern "C" fn(FrdDecoderHandle, *mut FrdDecodedFrame) -> FrdStatus,
        expected_error: VideoDecodeErrorCode,
    ) {
        let _guard = FAKE_FRAME_TEST_LOCK.lock().expect("fake frame 测试锁有效");
        RELEASE_COUNT.store(0, Ordering::SeqCst);
        FRAME_RECLAIM_CALL_COUNT.store(0, Ordering::SeqCst);
        let mut api = ready_fake_api();
        api.receive = receive as *const () as usize;
        let backend = FfmpegBackend::from_raw_api_for_test(api).expect("兼容 fake API 应加载");
        let config = main444_config();
        let access_unit = test_access_unit(&config);
        let mut decoder = backend.create(&config).expect("fake decoder 应可创建");

        let result = decoder.submit(access_unit);

        assert!(matches!(result, Err(ref error) if error.code() == expected_error));
        assert_eq!(FRAME_RECLAIM_CALL_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(RELEASE_COUNT.load(Ordering::SeqCst), 1);
    }

    unsafe extern "C" fn receive_malformed_partial_frame(
        _handle: FrdDecoderHandle,
        frame: *mut FrdDecodedFrame,
    ) -> FrdStatus {
        if frame.is_null() {
            return FrdStatus::INVALID_ARGUMENT;
        }
        let output = FrdDecodedFrame {
            timestamp_ticks: 1,
            pixel_format: FRD_PIXEL_FORMAT_YUV_444_P8,
            plane_count: 1,
            planes: [
                fake_plane(0x10),
                FrdDecodedPlane::default(),
                FrdDecodedPlane::default(),
            ],
        };
        // SAFETY: caller provided host-owned writable output storage.
        unsafe { frame.write(output) };
        FrdStatus::OK
    }

    unsafe extern "C" fn receive_error_with_partial_frame(
        _handle: FrdDecoderHandle,
        frame: *mut FrdDecodedFrame,
    ) -> FrdStatus {
        if frame.is_null() {
            return FrdStatus::INVALID_ARGUMENT;
        }
        let output = FrdDecodedFrame {
            timestamp_ticks: 0,
            pixel_format: 0,
            plane_count: 1,
            planes: [
                fake_plane(0x10),
                FrdDecodedPlane::default(),
                FrdDecodedPlane::default(),
            ],
        };
        // SAFETY: caller provided host-owned writable output storage; error may be partial.
        unsafe { frame.write(output) };
        FrdStatus::DECODE_FAILED
    }

    fn test_access_unit(config: &VideoStreamConfig) -> EncodedVideoAccessUnit {
        let input = config.as_input();
        EncodedVideoAccessUnit::try_new(
            input.identity,
            input.generation,
            VideoTimestamp {
                ticks: 1,
                timescale: input.time_base.ticks_per_second(),
            },
            true,
            vec![0, 0, 0, 1, 0x26].into_boxed_slice(),
        )
        .expect("测试访问单元有效")
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

    unsafe extern "C" fn stub_reclaim_frame(
        _handle: FrdDecoderHandle,
        _frame: *mut FrdDecodedFrame,
    ) {
    }

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
