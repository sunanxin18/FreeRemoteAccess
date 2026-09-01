//! FreeRemoteDesk 与可选 FFmpeg 插件之间的 C ABI。
//!
//! 该边界只传递定宽整数、裸指针和函数指针。调用方传入的配置与码流只在调用期间有效，
//! 插件必须复制需延长生命周期的数据。插件返回的每个 buffer 都携带原分配模块的释放回调，
//! 产品侧不得使用 Rust 或本地 CRT allocator 直接释放它。

use core::ffi::c_void;

pub const FRD_FFMPEG_ABI_VERSION: u32 = 1;
pub const FRD_FFMPEG_AVCODEC_MAJOR: u32 = 62;
pub const FRD_FFMPEG_API_SYMBOL: &[u8] = b"frd_ffmpeg_get_api_v1\0";

pub const FRD_CODEC_HEVC: u32 = 1;
pub const FRD_PROFILE_HEVC_MAIN_444_8: u32 = 1;
pub const FRD_CHROMA_YUV_444: u32 = 1;
pub const FRD_BITSTREAM_ANNEX_B: u32 = 1;
pub const FRD_PIXEL_FORMAT_YUV_444_P8: u32 = 1;
pub const FRD_SUBMIT_RANDOM_ACCESS: u32 = 1;

pub type FrdDecoderHandle = *mut c_void;
pub type FrdReleaseBufferFn = unsafe extern "C" fn(*mut c_void, *const u8, usize);

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrdStatus(pub i32);

impl FrdStatus {
    pub const OK: Self = Self(0);
    pub const NEED_MORE_DATA: Self = Self(1);
    pub const END_OF_STREAM: Self = Self(2);
    pub const UNSUPPORTED: Self = Self(-1);
    pub const INVALID_ARGUMENT: Self = Self(-2);
    pub const DECODE_FAILED: Self = Self(-3);
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FrdByteSlice {
    pub data: *const u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FrdVideoConfig {
    pub codec: u32,
    pub profile: u32,
    pub chroma: u32,
    pub bit_depth: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub timebase: u32,
    pub bitstream_format: u32,
    pub vps: FrdByteSlice,
    pub sps: FrdByteSlice,
    pub pps: FrdByteSlice,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FrdOwnedBuffer {
    pub data: *const u8,
    pub len: usize,
    pub release: Option<FrdReleaseBufferFn>,
    pub release_context: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FrdDecodedPlane {
    pub width: u32,
    pub height: u32,
    pub stride_bytes: u32,
    pub buffer: FrdOwnedBuffer,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FrdDecodedFrame {
    pub timestamp_ticks: i64,
    pub pixel_format: u32,
    pub plane_count: u32,
    pub planes: [FrdDecodedPlane; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrdFfmpegApiV1 {
    pub abi_version: u32,
    pub avcodec_major: u32,
    pub create_decoder:
        unsafe extern "C" fn(*const FrdVideoConfig, *mut FrdDecoderHandle) -> FrdStatus,
    pub submit: unsafe extern "C" fn(FrdDecoderHandle, *const u8, usize, i64, u32) -> FrdStatus,
    pub receive: unsafe extern "C" fn(FrdDecoderHandle, *mut FrdDecodedFrame) -> FrdStatus,
    pub flush: unsafe extern "C" fn(FrdDecoderHandle) -> FrdStatus,
    pub destroy: unsafe extern "C" fn(FrdDecoderHandle),
}

pub type FrdGetFfmpegApiV1 = unsafe extern "C" fn() -> *const FrdFfmpegApiV1;
