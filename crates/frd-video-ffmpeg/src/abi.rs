//! FreeRemoteDesk 与可选 FFmpeg 插件之间的 C ABI。
//!
//! 该边界只传递定宽整数、裸指针和函数指针。调用方传入的配置与码流只在调用期间有效，
//! 插件必须复制需延长生命周期的数据。插件必须用 `reclaim_frame` 回收每次 `receive` 的
//! 零值、部分或完整输出；产品侧不得用 Rust 或本地 CRT allocator 直接释放跨模块 buffer。

use core::ffi::c_void;
use core::mem::{align_of, size_of};

pub const FRD_FFMPEG_ABI_VERSION: u32 = 1;
pub const FRD_FFMPEG_AVCODEC_MAJOR: u32 = 62;
pub const FRD_FFMPEG_API_SYMBOL: &[u8] = b"frd_ffmpeg_get_api_v1\0";

pub const FRD_CODEC_HEVC: u32 = 1;
pub const FRD_PROFILE_HEVC_MAIN_444_8: u32 = 1;
pub const FRD_CHROMA_YUV_444: u32 = 1;
pub const FRD_BITSTREAM_ANNEX_B: u32 = 1;
pub const FRD_PIXEL_FORMAT_YUV_444_P8: u32 = 1;
pub const FRD_SUBMIT_RANDOM_ACCESS: u32 = 1;

/// Decoder handle may migrate between threads while remaining exclusively owned and serialized.
pub const FRD_CONTRACT_HANDLE_MIGRATION_SAFE: u32 = 1 << 0;
/// Global/factory callbacks, including `create_decoder`, may be called concurrently.
pub const FRD_CONTRACT_THREAD_SAFE_CREATE: u32 = 1 << 1;
/// Every non-null create output handle is host-owned and destroyable, regardless of status.
pub const FRD_CONTRACT_CREATE_NON_NULL_OWNED: u32 = 1 << 2;
/// `reclaim_frame` accepts host-zeroed, partial, success and error receive outputs exactly once.
pub const FRD_CONTRACT_RECLAIM_ANY_RECEIVE_STATUS: u32 = 1 << 3;
pub const FRD_API_CONTRACT_REQUIRED: u32 = FRD_CONTRACT_HANDLE_MIGRATION_SAFE
    | FRD_CONTRACT_THREAD_SAFE_CREATE
    | FRD_CONTRACT_CREATE_NON_NULL_OWNED
    | FRD_CONTRACT_RECLAIM_ANY_RECEIVE_STATUS;

pub type FrdDecoderHandle = *mut c_void;
/// On every return status, a non-null output handle transfers to the host and must be accepted by
/// `FrdDestroyFn` exactly once. Success must return a non-null handle.
pub type FrdCreateDecoderFn =
    unsafe extern "C" fn(*const FrdVideoConfig, *mut FrdDecoderHandle) -> FrdStatus;
pub type FrdSubmitFn =
    unsafe extern "C" fn(FrdDecoderHandle, *const u8, usize, i64, u32) -> FrdStatus;
pub type FrdReceiveFn = unsafe extern "C" fn(FrdDecoderHandle, *mut FrdDecodedFrame) -> FrdStatus;
pub type FrdFlushFn = unsafe extern "C" fn(FrdDecoderHandle) -> FrdStatus;
pub type FrdDestroyFn = unsafe extern "C" fn(FrdDecoderHandle);
/// Called exactly once after every `FrdReceiveFn` result. It must accept host-zeroed output and any
/// partial fields produced on success or failure, free all plugin allocations, and tolerate no
/// host interpretation of an error-status frame.
pub type FrdReclaimFrameFn = unsafe extern "C" fn(FrdDecoderHandle, *mut FrdDecodedFrame);

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
#[derive(Clone, Copy, Debug, Default)]
pub struct FrdOwnedBuffer {
    /// On an OK receive result this is either null for an invalid frame or readable for `len`
    /// bytes until the matching frame-level reclaim callback returns.
    pub data: *const u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FrdDecodedPlane {
    pub width: u32,
    pub height: u32,
    pub stride_bytes: u32,
    pub buffer: FrdOwnedBuffer,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FrdDecodedFrame {
    pub timestamp_ticks: i64,
    pub pixel_format: u32,
    pub plane_count: u32,
    pub planes: [FrdDecodedPlane; 3],
}

/// Plugin-populated ABI table. Callback addresses remain integers until every header, contract
/// flag and required slot has been validated, so zero or arbitrary C output is always valid Rust
/// data and cannot trigger invalid-function-pointer UB merely by being read.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RawFrdFfmpegApiV1 {
    pub struct_size: u32,
    pub struct_alignment: u32,
    pub abi_version: u32,
    pub avcodec_major: u32,
    pub contract_flags: u32,
    pub reserved: u32,
    pub create_decoder: usize,
    pub submit: usize,
    pub receive: usize,
    pub flush: usize,
    pub destroy: usize,
    pub reclaim_frame: usize,
}

pub const FRD_FFMPEG_API_V1_SIZE: u32 = size_of::<RawFrdFfmpegApiV1>() as u32;
pub const FRD_FFMPEG_API_V1_ALIGNMENT: u32 = align_of::<RawFrdFfmpegApiV1>() as u32;

/// The host passes aligned, zeroed storage and its exact byte length. A plugin must never write
/// beyond `output_size`; unavailable plugins leave the output zeroed and return non-OK.
pub type FrdGetFfmpegApiV1 = unsafe extern "C" fn(*mut RawFrdFfmpegApiV1, usize) -> FrdStatus;
