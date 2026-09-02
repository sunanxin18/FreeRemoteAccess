#![cfg(windows)]

use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use frd_video_ffmpeg::abi::{
    FrdByteSlice, FrdCreateDecoderFn, FrdDecodedFrame, FrdDestroyFn, FrdFlushFn, FrdGetFfmpegApiV1,
    FrdReceiveFn, FrdReclaimFrameFn, FrdStatus, FrdSubmitFn, FrdVideoConfig, RawFrdFfmpegApiV1,
    FRD_API_CONTRACT_REQUIRED, FRD_BITSTREAM_ANNEX_B, FRD_CHROMA_YUV_444, FRD_CODEC_HEVC,
    FRD_FFMPEG_ABI_VERSION, FRD_FFMPEG_API_SYMBOL, FRD_FFMPEG_API_V1_ALIGNMENT,
    FRD_FFMPEG_API_V1_SIZE, FRD_FFMPEG_AVCODEC_MAJOR, FRD_PIXEL_FORMAT_YUV_444_P8,
    FRD_PROFILE_HEVC_MAIN_444_8, FRD_SUBMIT_RANDOM_ACCESS,
};
#[derive(Debug)]
struct FixtureMetadata {
    codec: String,
    profile: String,
    chroma: String,
    bit_depth: u32,
    bitstream_format: String,
    coded_width: u32,
    coded_height: u32,
    visible_width: u32,
    visible_height: u32,
    frame_count: usize,
    plane_stride_bytes: u32,
    plane_sha256: PlaneChecksums,
}

#[derive(Debug)]
struct PlaneChecksums {
    y: String,
    u: String,
    v: String,
}

#[derive(Clone, Copy)]
struct Api {
    create: FrdCreateDecoderFn,
    submit: FrdSubmitFn,
    receive: FrdReceiveFn,
    flush: FrdFlushFn,
    destroy: FrdDestroyFn,
    reclaim: FrdReclaimFrameFn,
}

#[test]
fn fixed_ffmpeg_decodes_synthetic_main444_idr_to_owned_yuv444p8() {
    let metadata = fixture_metadata(include_str!("fixtures/apple-main444-idr.json"));
    assert_fixture_metadata(&metadata);

    let bitstream = include_bytes!("fixtures/apple-main444-idr.hevc");
    let nals = annex_b_nals(bitstream);
    assert_eq!(
        nals.iter().map(|nal| nal.kind).collect::<Vec<_>>(),
        vec![32, 33, 34, 20],
        "fixture 必须只含 VPS/SPS/PPS 和单个 IDR_N_LP"
    );

    let bundle = development_codec_bundle();
    let loaded = unsafe { load_direct_api(&bundle) };
    let api = loaded.api;
    let config = FrdVideoConfig {
        codec: FRD_CODEC_HEVC,
        profile: FRD_PROFILE_HEVC_MAIN_444_8,
        chroma: FRD_CHROMA_YUV_444,
        bit_depth: metadata.bit_depth,
        coded_width: metadata.coded_width,
        coded_height: metadata.coded_height,
        timebase: 90_000,
        bitstream_format: FRD_BITSTREAM_ANNEX_B,
        vps: byte_slice(nals[0].bytes),
        sps: byte_slice(nals[1].bytes),
        pps: byte_slice(nals[2].bytes),
    };

    let mut handle = std::ptr::null_mut();
    // SAFETY: config and output storage remain valid for the call; the validated table owns the
    // returned handle until the matching destroy callback below.
    let create_status = unsafe { (api.create)(&config, &mut handle) };
    assert_eq!(create_status, FrdStatus::OK);
    assert!(!handle.is_null());

    let access_unit = with_start_code(nals[3].bytes);
    // SAFETY: handle is exclusively owned and serialized; input bytes live through the call.
    let submit_status = unsafe {
        (api.submit)(
            handle,
            access_unit.as_ptr(),
            access_unit.len(),
            90_000,
            FRD_SUBMIT_RANDOM_ACCESS,
        )
    };
    assert!(
        submit_status == FrdStatus::OK || submit_status == FrdStatus::NEED_MORE_DATA,
        "submit 应接受经验证的 Annex-B IDR，实际 {submit_status:?}"
    );

    let decoded = unsafe { receive_one_frame(api, handle) };
    assert_eq!(decoded.timestamp_ticks, 90_000);
    assert_eq!(decoded.pixel_format, FRD_PIXEL_FORMAT_YUV_444_P8);
    assert_eq!(decoded.plane_count, 3);

    let expected_hashes = [
        &metadata.plane_sha256.y,
        &metadata.plane_sha256.u,
        &metadata.plane_sha256.v,
    ];
    for (index, plane) in decoded.planes.iter().enumerate() {
        assert_eq!(plane.width, metadata.visible_width);
        assert_eq!(plane.height, metadata.visible_height);
        assert_eq!(plane.stride_bytes, metadata.plane_stride_bytes);
        assert_eq!(
            plane.buffer.len,
            usize::try_from(metadata.plane_stride_bytes * metadata.visible_height).unwrap()
        );
        assert!(!plane.buffer.data.is_null());
        // SAFETY: successful receive promises readable plugin-owned storage until reclaim.
        let bytes = unsafe { std::slice::from_raw_parts(plane.buffer.data, plane.buffer.len) };
        assert_eq!(hex_sha256(bytes), *expected_hashes[index]);
    }

    let mut decoded = decoded;
    // SAFETY: exactly-once reclamation of the successful receive output.
    unsafe { (api.reclaim)(handle, &mut decoded) };
    assert!(
        decoded
            .planes
            .iter()
            .all(|plane| plane.buffer.data.is_null() && plane.buffer.len == 0),
        "reclaim 必须清空所有已释放的 plugin buffer"
    );

    let mut drained = FrdDecodedFrame::default();
    // SAFETY: handle remains valid and output starts zeroed.
    let status = unsafe { (api.receive)(handle, &mut drained) };
    assert!(status == FrdStatus::NEED_MORE_DATA || status == FrdStatus::END_OF_STREAM);
    // SAFETY: ABI requires reclaim after every receive status, including zero output.
    unsafe { (api.reclaim)(handle, &mut drained) };
    // SAFETY: final release of the exclusive handle.
    unsafe { (api.destroy)(handle) };
}

#[test]
fn fixed_ffmpeg_preserves_coded_planes_for_synthetic_cropped_main444_idr() {
    const CODED_WIDTH: u32 = 16;
    const CODED_HEIGHT: u32 = 16;
    const PLANE_STRIDE_BYTES: u32 = 16;

    let bitstream = include_bytes!("fixtures/synthetic-main444-cropped-16x8.hevc");
    let nals = annex_b_nals(bitstream);
    assert_eq!(
        nals.iter().map(|nal| nal.kind).collect::<Vec<_>>(),
        vec![32, 33, 34, 20],
        "裁剪 fixture 必须只含 VPS/SPS/PPS 和单个 IDR_N_LP"
    );

    let loaded = unsafe { load_direct_api(&development_codec_bundle()) };
    let api = loaded.api;
    let config = FrdVideoConfig {
        codec: FRD_CODEC_HEVC,
        profile: FRD_PROFILE_HEVC_MAIN_444_8,
        chroma: FRD_CHROMA_YUV_444,
        bit_depth: 8,
        coded_width: CODED_WIDTH,
        coded_height: CODED_HEIGHT,
        timebase: 90_000,
        bitstream_format: FRD_BITSTREAM_ANNEX_B,
        vps: byte_slice(nals[0].bytes),
        sps: byte_slice(nals[1].bytes),
        pps: byte_slice(nals[2].bytes),
    };

    let mut handle = std::ptr::null_mut();
    // SAFETY: config and output storage remain valid for the call; the validated table owns the
    // returned handle until the matching destroy callback below.
    assert_eq!(unsafe { (api.create)(&config, &mut handle) }, FrdStatus::OK);
    assert!(!handle.is_null());

    let access_unit = with_start_code(nals[3].bytes);
    // SAFETY: handle is exclusively owned and serialized; input bytes live through the call.
    let submit_status = unsafe {
        (api.submit)(
            handle,
            access_unit.as_ptr(),
            access_unit.len(),
            90_000,
            FRD_SUBMIT_RANDOM_ACCESS,
        )
    };
    assert!(
        submit_status == FrdStatus::OK || submit_status == FrdStatus::NEED_MORE_DATA,
        "submit 应接受经验证的 Annex-B IDR，实际 {submit_status:?}"
    );

    let mut decoded = unsafe { receive_one_frame(api, handle) };
    assert_eq!(decoded.timestamp_ticks, 90_000);
    assert_eq!(decoded.pixel_format, FRD_PIXEL_FORMAT_YUV_444_P8);
    assert_eq!(decoded.plane_count, 3);
    for plane in decoded.planes {
        assert_eq!(plane.width, CODED_WIDTH);
        assert_eq!(plane.height, CODED_HEIGHT);
        assert_eq!(plane.stride_bytes, PLANE_STRIDE_BYTES);
        assert_eq!(
            plane.buffer.len,
            usize::try_from(PLANE_STRIDE_BYTES * CODED_HEIGHT).unwrap()
        );
        assert!(!plane.buffer.data.is_null());
    }
    // SAFETY: exactly-once reclamation of the successful receive output.
    unsafe { (api.reclaim)(handle, &mut decoded) };
    // SAFETY: final release of the exclusive handle.
    unsafe { (api.destroy)(handle) };
}

#[test]
fn native_callbacks_fail_closed_for_out_of_contract_inputs() {
    let metadata = fixture_metadata(include_str!("fixtures/apple-main444-idr.json"));
    let nals = annex_b_nals(include_bytes!("fixtures/apple-main444-idr.hevc"));
    let loaded = unsafe { load_direct_api(&development_codec_bundle()) };
    let api = loaded.api;
    let mut config = FrdVideoConfig {
        codec: FRD_CODEC_HEVC,
        profile: FRD_PROFILE_HEVC_MAIN_444_8,
        chroma: FRD_CHROMA_YUV_444,
        bit_depth: metadata.bit_depth,
        coded_width: metadata.coded_width,
        coded_height: metadata.coded_height,
        timebase: 90_000,
        bitstream_format: FRD_BITSTREAM_ANNEX_B,
        vps: byte_slice(nals[0].bytes),
        sps: byte_slice(nals[1].bytes),
        pps: byte_slice(nals[2].bytes),
    };

    config.coded_width = 8193;
    let mut rejected_handle = 1usize as *mut core::ffi::c_void;
    // SAFETY: valid storage; callback must clear the output before rejecting dimensions.
    assert_eq!(
        unsafe { (api.create)(&config, &mut rejected_handle) },
        FrdStatus::INVALID_ARGUMENT
    );
    assert!(rejected_handle.is_null());

    config.coded_width = metadata.coded_width;
    config.bit_depth = 10;
    // SAFETY: same valid storage, unsupported exactness tuple.
    assert_eq!(
        unsafe { (api.create)(&config, &mut rejected_handle) },
        FrdStatus::UNSUPPORTED
    );
    assert!(rejected_handle.is_null());

    config.bit_depth = 8;
    config.pps = FrdByteSlice {
        data: std::ptr::null(),
        len: 0,
    };
    // SAFETY: malformed parameter set is rejected without dereference.
    assert_eq!(
        unsafe { (api.create)(&config, &mut rejected_handle) },
        FrdStatus::INVALID_ARGUMENT
    );
    assert!(rejected_handle.is_null());

    config.pps = byte_slice(nals[2].bytes);
    let mut handle = std::ptr::null_mut();
    // SAFETY: restored valid config.
    assert_eq!(unsafe { (api.create)(&config, &mut handle) }, FrdStatus::OK);
    assert!(!handle.is_null());
    // SAFETY: invalid pointer/length tuple is rejected before native access.
    assert_eq!(
        unsafe { (api.submit)(handle, std::ptr::null(), 0, 0, 0) },
        FrdStatus::INVALID_ARGUMENT
    );
    let access_unit = with_start_code(nals[3].bytes);
    // SAFETY: unknown flags are rejected before the valid input is passed downstream.
    assert_eq!(
        unsafe { (api.submit)(handle, access_unit.as_ptr(), access_unit.len(), 0, u32::MAX,) },
        FrdStatus::INVALID_ARGUMENT
    );
    // SAFETY: null output must be rejected without changing decoder ownership.
    assert_eq!(
        unsafe { (api.receive)(handle, std::ptr::null_mut()) },
        FrdStatus::INVALID_ARGUMENT
    );
    let mut zero = FrdDecodedFrame::default();
    // SAFETY: zero/partial/error outputs must be accepted by reclaim even with no allocation.
    unsafe { (api.reclaim)(handle, &mut zero) };
    assert!(zero
        .planes
        .iter()
        .all(|plane| plane.buffer.data.is_null() && plane.buffer.len == 0));
    // SAFETY: release the valid exclusive handle once; destroying null is also required to be safe.
    unsafe {
        (api.destroy)(handle);
        (api.destroy)(std::ptr::null_mut());
    }
}

unsafe fn receive_one_frame(api: Api, handle: *mut core::ffi::c_void) -> FrdDecodedFrame {
    let mut frame = FrdDecodedFrame::default();
    // SAFETY: caller supplies a valid exclusive handle and zeroed writable output.
    let first_status = unsafe { (api.receive)(handle, &mut frame) };
    if first_status == FrdStatus::OK {
        return frame;
    }
    assert_eq!(first_status, FrdStatus::NEED_MORE_DATA);
    // SAFETY: every receive result is reclaimed, including the zero EAGAIN output.
    unsafe { (api.reclaim)(handle, &mut frame) };
    // SAFETY: flushing the exclusive decoder is serialized after submit.
    let flush_status = unsafe { (api.flush)(handle) };
    assert!(flush_status == FrdStatus::OK || flush_status == FrdStatus::END_OF_STREAM);
    let mut frame = FrdDecodedFrame::default();
    // SAFETY: same valid handle and fresh zeroed output.
    let status = unsafe { (api.receive)(handle, &mut frame) };
    assert_eq!(status, FrdStatus::OK, "flush 后必须产出 fixture 帧");
    frame
}

struct LoadedApi {
    api: Api,
    // Drop order is intentional: unload the plugin before the two libraries it imports.
    _plugin: Module,
    _avcodec: Module,
    _avutil: Module,
}

struct Module(*mut core::ffi::c_void);

impl Drop for Module {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this handle came from `LoadLibraryW` and is released exactly once.
            unsafe { FreeLibrary(self.0) };
        }
    }
}

unsafe extern "system" {
    fn LoadLibraryW(path: *const u16) -> *mut core::ffi::c_void;
    fn GetProcAddress(module: *mut core::ffi::c_void, name: *const u8) -> *mut core::ffi::c_void;
    fn FreeLibrary(module: *mut core::ffi::c_void) -> i32;
}

unsafe fn load_direct_api(bundle: &Path) -> LoadedApi {
    assert!(
        bundle.is_absolute(),
        "测试 bundle 必须是绝对路径: {}",
        bundle.display()
    );
    // SAFETY: these are absolute paths into the test-only ignored build output. Product trust and
    // ACL policy is not changed or bypassed by product code.
    let avutil = unsafe { load_module(&bundle.join("avutil-60.dll")) };
    // SAFETY: avutil remains loaded while avcodec and the plugin are alive.
    let avcodec = unsafe { load_module(&bundle.join("avcodec-62.dll")) };
    // SAFETY: same test-only bundle, held for the entire callback lifetime.
    let plugin = unsafe { load_module(&bundle.join("freeremotedesk_ffmpeg.dll")) };
    // SAFETY: the symbol name and C signature are the versioned Task 4 ABI.
    let symbol = unsafe { GetProcAddress(plugin.0, FRD_FFMPEG_API_SYMBOL.as_ptr()) };
    assert!(
        !symbol.is_null(),
        "native plugin 必须导出 frd_ffmpeg_get_api_v1"
    );
    // SAFETY: the exported symbol has the versioned C signature by contract.
    let get_api = unsafe { mem::transmute::<*mut core::ffi::c_void, FrdGetFfmpegApiV1>(symbol) };
    let mut raw = RawFrdFfmpegApiV1::default();
    let status = get_api(&mut raw, mem::size_of::<RawFrdFfmpegApiV1>());
    assert_eq!(status, FrdStatus::OK, "native plugin 必须提供完整 API 表");
    assert_eq!(raw.struct_size, FRD_FFMPEG_API_V1_SIZE);
    assert_eq!(raw.struct_alignment, FRD_FFMPEG_API_V1_ALIGNMENT);
    assert_eq!(raw.abi_version, FRD_FFMPEG_ABI_VERSION);
    assert_eq!(raw.avcodec_major, FRD_FFMPEG_AVCODEC_MAJOR);
    assert_eq!(
        raw.contract_flags & FRD_API_CONTRACT_REQUIRED,
        FRD_API_CONTRACT_REQUIRED
    );
    assert!([
        raw.create_decoder,
        raw.submit,
        raw.receive,
        raw.flush,
        raw.destroy,
        raw.reclaim_frame,
    ]
    .iter()
    .all(|slot| *slot != 0));
    let api = Api {
        // SAFETY: the size/version/flags and every non-zero callback slot were validated first.
        create: unsafe { mem::transmute::<usize, FrdCreateDecoderFn>(raw.create_decoder) },
        // SAFETY: same validated table.
        submit: unsafe { mem::transmute::<usize, FrdSubmitFn>(raw.submit) },
        // SAFETY: same validated table.
        receive: unsafe { mem::transmute::<usize, FrdReceiveFn>(raw.receive) },
        // SAFETY: same validated table.
        flush: unsafe { mem::transmute::<usize, FrdFlushFn>(raw.flush) },
        // SAFETY: same validated table.
        destroy: unsafe { mem::transmute::<usize, FrdDestroyFn>(raw.destroy) },
        // SAFETY: same validated table.
        reclaim: unsafe { mem::transmute::<usize, FrdReclaimFrameFn>(raw.reclaim_frame) },
    };
    LoadedApi {
        api,
        _plugin: plugin,
        _avcodec: avcodec,
        _avutil: avutil,
    }
}

unsafe fn load_module(path: &Path) -> Module {
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide.push(0);
    // SAFETY: path is a terminated UTF-16 string and the returned handle is owned by `Module`.
    let handle = unsafe { LoadLibraryW(wide.as_ptr()) };
    assert!(!handle.is_null(), "无法加载测试 DLL: {}", path.display());
    Module(handle)
}

fn development_codec_bundle() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".codex-target/ffmpeg-8.1.2/windows-x86_64/Release/codec")
        .canonicalize()
        .expect("请先运行 tools/build-ffmpeg-windows.ps1 生成测试 bundle")
}

fn assert_fixture_metadata(metadata: &FixtureMetadata) {
    assert_eq!(metadata.codec, "hevc");
    assert_eq!(metadata.profile, "main444-8");
    assert_eq!(metadata.chroma, "yuv444");
    assert_eq!(metadata.bit_depth, 8);
    assert_eq!(metadata.bitstream_format, "annex-b");
    assert_eq!((metadata.coded_width, metadata.coded_height), (16, 16));
    assert_eq!((metadata.visible_width, metadata.visible_height), (16, 16));
    assert_eq!(metadata.frame_count, 1);
    assert_eq!(metadata.plane_stride_bytes, 16);
}

fn fixture_metadata(json: &str) -> FixtureMetadata {
    FixtureMetadata {
        codec: json_string(json, "codec"),
        profile: json_string(json, "profile"),
        chroma: json_string(json, "chroma"),
        bit_depth: json_u32(json, "bit_depth"),
        bitstream_format: json_string(json, "bitstream_format"),
        coded_width: json_u32(json, "coded_width"),
        coded_height: json_u32(json, "coded_height"),
        visible_width: json_u32(json, "visible_width"),
        visible_height: json_u32(json, "visible_height"),
        frame_count: usize::try_from(json_u32(json, "frame_count")).unwrap(),
        plane_stride_bytes: json_u32(json, "plane_stride_bytes"),
        plane_sha256: PlaneChecksums {
            y: json_string(json, "y"),
            u: json_string(json, "u"),
            v: json_string(json, "v"),
        },
    }
}

fn json_value_tail<'a>(json: &'a str, key: &str) -> &'a str {
    let marker = format!("\"{key}\"");
    let tail = json
        .find(&marker)
        .map(|offset| &json[offset + marker.len()..])
        .unwrap_or_else(|| panic!("fixture JSON 缺少字段 {key}"));
    tail.strip_prefix(':')
        .or_else(|| tail.trim_start().strip_prefix(':'))
        .expect("JSON 字段名后必须有冒号")
        .trim_start()
}

fn json_string(json: &str, key: &str) -> String {
    let tail = json_value_tail(json, key);
    let value = tail
        .strip_prefix('"')
        .and_then(|tail| tail.split_once('"').map(|(value, _)| value))
        .unwrap_or_else(|| panic!("fixture JSON 字段 {key} 必须是简单字符串"));
    value.to_owned()
}

fn json_u32(json: &str, key: &str) -> u32 {
    let value = json_value_tail(json, key)
        .split(|character: char| !character.is_ascii_digit())
        .next()
        .unwrap_or_default();
    value
        .parse()
        .unwrap_or_else(|_| panic!("fixture JSON 字段 {key} 必须是非负整数"))
}

fn byte_slice(bytes: &[u8]) -> FrdByteSlice {
    FrdByteSlice {
        data: bytes.as_ptr(),
        len: bytes.len(),
    }
}

fn with_start_code(nal: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(4 + nal.len());
    bytes.extend_from_slice(&[0, 0, 0, 1]);
    bytes.extend_from_slice(nal);
    bytes
}

#[derive(Clone, Copy)]
struct Nal<'a> {
    kind: u8,
    bytes: &'a [u8],
}

fn annex_b_nals(input: &[u8]) -> Vec<Nal<'_>> {
    let mut starts = Vec::new();
    let mut offset = 0usize;
    while offset + 3 <= input.len() {
        let prefix = if input[offset..].starts_with(&[0, 0, 0, 1]) {
            Some(4)
        } else if input[offset..].starts_with(&[0, 0, 1]) {
            Some(3)
        } else {
            None
        };
        if let Some(prefix_len) = prefix {
            starts.push((offset, prefix_len));
            offset += prefix_len;
        } else {
            offset += 1;
        }
    }

    starts
        .iter()
        .enumerate()
        .map(|(index, &(start, prefix_len))| {
            let payload_start = start + prefix_len;
            let end = starts
                .get(index + 1)
                .map(|&(next, _)| next)
                .unwrap_or(input.len());
            let bytes = &input[payload_start..end];
            assert!(bytes.len() >= 2, "HEVC NAL header 必须完整");
            Nal {
                kind: (bytes[0] >> 1) & 0x3f,
                bytes,
            }
        })
        .collect()
}

fn hex_sha256(bytes: &[u8]) -> String {
    sha256(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (input.len() as u64) * 8;
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut hash = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (index, bytes) in chunk.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(bytes.try_into().unwrap());
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = hash;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (state, value) in hash.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *state = state.wrapping_add(value);
        }
    }

    let mut output = [0u8; 32];
    for (chunk, value) in output.chunks_exact_mut(4).zip(hash) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    output
}
